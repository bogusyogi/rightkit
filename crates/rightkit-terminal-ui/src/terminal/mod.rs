//! Terminal ownership: capability detection, background lightness, scrollback
//! insertion, and the inline/alt-screen surface.

mod alternate_screen;
pub mod capability;
/// Donor custom terminal: dual-buffer diff with full-cell drawn bounds,
/// cursor repair, and history-aware viewport accounting.
pub mod custom_terminal;
pub mod external_editor;
pub mod glyphs;
pub mod history;
pub mod hyperlinks;
mod input_boundary;
mod keyboard_modes;
pub mod line_utils;
mod link_pointer;
pub mod notify;
pub mod scrollback;
/// Session modes and restoration, ported from the donor `tui.rs` lifecycle.
/// The host owns when to call `set_modes`/`restore`; this module owns the
/// exact escape discipline.
pub mod session;
#[cfg(test)]
mod test_backend;
mod tmux;
#[cfg(any(windows, test))]
mod windows_console;

/// Text of one visible screen row (0-based from the window top), trailing
/// blanks trimmed. Only a Windows console can be read back; elsewhere `None`.
pub fn visible_row_text(row: u16, columns: u16) -> Option<String> {
    #[cfg(windows)]
    {
        windows_console::read_visible_row(row, columns)
    }
    #[cfg(not(windows))]
    {
        let _ = (row, columns);
        None
    }
}

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

pub use capability::{
    Capabilities, ClipboardStrategy, ColorDepth, ImageProtocol, Multiplexer,
    clipboard_osc52_for_env, clipboard_strategy, is_ssh_session, osc52_sequence,
    osc52_tmux_sequence,
};
pub use history::{InsertMode, Viewport};

/// S:I4 — write `text` to the system clipboard via OSC52 with the codex
/// fallback chain (SSH → tmux-wrapped → plain). Returns the bytes written
/// so callers can distinguish success (non-zero) from a terminal that
/// ignored the sequence. Purely best-effort: never errors.
pub fn copy_via_osc52(
    out: &mut impl Write,
    text: &str,
    env: &impl capability::Env,
) -> io::Result<usize> {
    const MAX_RAW_BYTES: usize = 100_000;
    if text.len() > MAX_RAW_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("OSC52 payload exceeds {MAX_RAW_BYTES} bytes"),
        ));
    }
    let seq = clipboard_osc52_for_env(text, env);
    out.write_all(seq.as_bytes())?;
    out.flush()?;
    Ok(seq.len())
}

/// Whether the terminal is painting us onto a light or dark background.
///
/// This is the difference between a readable UI and an unreadable one: a
/// palette tuned for dark backgrounds is invisible on light, and vice versa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Background {
    Light,
    Dark,
}

impl Background {
    /// The fallback when nothing can be determined.
    ///
    /// Dark, because it is the more common terminal default and because a light
    /// palette on a dark background is harder to read than the reverse.
    pub const FALLBACK: Self = Self::Dark;
}

/// How the background was decided, so `doctor` can explain a wrong guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundSource {
    /// The user said so. Always wins.
    UserSetting,
    /// The terminal answered an OSC 11 query.
    Queried,
    /// Nothing answered; [`Background::FALLBACK`] was used.
    Fallback,
}

/// How long to wait for a terminal to answer OSC 11.
///
/// Long enough for a local terminal or a responsive SSH session, short enough
/// that a silent terminal does not visibly delay startup. The query runs on a
/// worker thread anyway, so this bounds a background task, not the UI.
pub const BACKGROUND_QUERY_TIMEOUT: Duration = Duration::from_millis(120);

/// `TERM` values known never to answer OSC queries. Querying them wastes the
/// full timeout on every launch and can leave a stray escape in the input
/// stream, so skip straight to the fallback.
const SILENT_TERMINALS: &[&str] = &["", "dumb", "linux", "cons25", "emacs"];

/// Whether it is worth asking this terminal about its background.
pub fn background_query_supported(env: &impl capability::Env) -> bool {
    let term = env.get("TERM").unwrap_or_default().to_ascii_lowercase();
    if !SILENT_TERMINALS.contains(&term.as_str()) {
        return true;
    }
    // A known-silent TERM can still be a modern terminal misreporting itself;
    // a program identity is good enough evidence to try.
    env.get("TERM_PROGRAM").is_some() || env.get("LC_TERMINAL").is_some()
}

/// The OSC 11 query: "report the background colour".
///
/// Deliberately not paired with an OSC 10 (foreground) query. Sending both
/// invites two replies whose interleaving is terminal-dependent, and a
/// half-consumed reply leaks into the key parser as garbage input.
pub const OSC11_QUERY: &[u8] = b"\x1b]11;?\x1b\\";

/// Parse an OSC 11 reply into 8-bit RGB.
///
/// Terminals reply in several shapes: `rgb:RRRR/GGGG/BBBB` is most common, but
/// 8-bit-per-channel and `#RRGGBB` forms appear too. Components may be 1-4 hex
/// digits and are scaled to 8 bits.
pub fn parse_osc11(reply: &str) -> Option<(u8, u8, u8)> {
    let body = reply.split("11;").nth(1)?;
    let body = body
        .trim_end_matches(['\x07', '\\', '\x1b'])
        .trim_end_matches(char::is_whitespace);

    if let Some(hex) = body.strip_prefix('#') {
        return parse_hex_triplet(hex);
    }
    let rest = body.strip_prefix("rgb:")?;
    let mut parts = rest.split('/');
    let r = scale_component(parts.next()?)?;
    let g = scale_component(parts.next()?)?;
    let b = scale_component(parts.next()?)?;
    Some((r, g, b))
}

fn parse_hex_triplet(hex: &str) -> Option<(u8, u8, u8)> {
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some((r, g, b))
}

/// Scale a 1-4 hex-digit component to 8 bits.
fn scale_component(part: &str) -> Option<u8> {
    let digits = part.trim();
    if digits.is_empty() || digits.len() > 4 {
        return None;
    }
    let value = u32::from_str_radix(digits, 16).ok()?;
    let max = 16u32.pow(digits.len() as u32) - 1;
    Some(((value * 255) / max) as u8)
}

/// Classify a background colour as light or dark.
///
/// Uses perceived luminance rather than raw average: green contributes most of
/// what the eye reads as brightness, blue least.
pub fn classify(rgb: (u8, u8, u8)) -> Background {
    let (r, g, b) = rgb;
    let luminance = (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000;
    if luminance > 127 {
        Background::Light
    } else {
        Background::Dark
    }
}

/// Ask the terminal for its background colour.
///
/// Returns `None` if the terminal does not answer within
/// [`BACKGROUND_QUERY_TIMEOUT`]. The caller must already have the terminal in
/// raw mode, or the reply will be line-buffered and arrive too late.
///
/// Never call this on a terminal inherited in raw mode from a parent process
/// (a resumed session): the reply races whatever the parent was reading.
pub fn query_background(
    out: &mut impl Write,
    input: &mut impl Read,
    timeout: Duration,
) -> io::Result<Option<Background>> {
    out.write_all(OSC11_QUERY)?;
    out.flush()?;

    let deadline = Instant::now() + timeout;
    let mut buffer = Vec::with_capacity(64);
    let mut chunk = [0u8; 32];

    while Instant::now() < deadline {
        match input.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                if let Ok(text) = std::str::from_utf8(&buffer)
                    && let Some(rgb) = parse_osc11(text)
                {
                    return Ok(Some(classify(rgb)));
                }
                // Cap the buffer: a terminal streaming unrelated input should
                // not grow this without bound.
                if buffer.len() > 1024 {
                    break;
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

/// Everything the UI needs to know about its host terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalProfile {
    pub capabilities: Capabilities,
    pub background: Background,
    pub background_source: BackgroundSource,
    pub insert_mode: InsertMode,
}

impl TerminalProfile {
    /// Build a profile from the environment alone, without querying.
    ///
    /// Used when a query is impossible or undesirable -- a resumed session, a
    /// non-tty, or an explicit user setting. Always succeeds.
    pub fn from_env(env: &impl capability::Env, background: Option<Background>) -> Self {
        let capabilities = capability::detect(env);
        glyphs::set_ascii_only(!capabilities.unicode);
        let (background, background_source) = match background {
            Some(b) => (b, BackgroundSource::UserSetting),
            None => (Background::FALLBACK, BackgroundSource::Fallback),
        };
        Self {
            // Every Windows console host sits behind ConPTY (Windows Terminal,
            // conhost, or an embedding PTY), and ConPTY discards rows scrolled
            // out of a partial DECSTBM region instead of pushing them into
            // scrollback. WT_SESSION only marks Windows Terminal, so key the
            // newline-scrolling path on the OS, not on that variable.
            insert_mode: history::mode_for(
                capabilities.multiplexer,
                capabilities.windows_terminal || cfg!(windows),
            ),
            capabilities,
            background,
            background_source,
        }
    }

    /// Replace the background with a queried result.
    pub fn with_queried_background(mut self, background: Background) -> Self {
        self.background = background;
        self.background_source = BackgroundSource::Queried;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn known_silent_terminals_are_skipped_unless_they_name_a_program() {
        assert!(!background_query_supported(&env(&[("TERM", "dumb")])));
        assert!(!background_query_supported(&env(&[("TERM", "linux")])));
        assert!(background_query_supported(&env(&[(
            "TERM",
            "xterm-256color"
        )])));
        // A misreporting modern terminal still gets asked.
        assert!(background_query_supported(&env(&[
            ("TERM", "dumb"),
            ("TERM_PROGRAM", "WezTerm")
        ])));
    }

    #[test]
    fn osc11_replies_parse_in_every_common_shape() {
        // 16-bit components, ST-terminated.
        assert_eq!(
            parse_osc11("\x1b]11;rgb:0000/0000/0000\x1b\\"),
            Some((0, 0, 0))
        );
        assert_eq!(
            parse_osc11("\x1b]11;rgb:ffff/ffff/ffff\x1b\\"),
            Some((255, 255, 255))
        );
        // 8-bit components, BEL-terminated.
        assert_eq!(parse_osc11("\x1b]11;rgb:1e/1e/2e\x07"), Some((30, 30, 46)));
        // Hash form.
        assert_eq!(parse_osc11("\x1b]11;#ffffff\x1b\\"), Some((255, 255, 255)));
    }

    #[test]
    fn malformed_replies_are_rejected_rather_than_guessed() {
        assert_eq!(parse_osc11("garbage"), None);
        assert_eq!(parse_osc11("\x1b]11;rgb:zz/zz/zz\x07"), None);
        assert_eq!(parse_osc11("\x1b]11;rgb:00/00\x07"), None);
        assert_eq!(parse_osc11("\x1b]11;#fff\x1b\\"), None);
    }

    #[test]
    fn classification_follows_perceived_luminance_not_raw_average() {
        assert_eq!(classify((0, 0, 0)), Background::Dark);
        assert_eq!(classify((255, 255, 255)), Background::Light);
        // Pure blue is dark despite a high channel value.
        assert_eq!(classify((0, 0, 255)), Background::Dark);
        // Pure green reads bright.
        assert_eq!(classify((0, 255, 0)), Background::Light);
    }

    #[test]
    fn a_silent_terminal_yields_none_rather_than_hanging() {
        let mut out = Vec::new();
        let mut input = io::empty();
        let result = query_background(&mut out, &mut input, Duration::from_millis(5)).unwrap();
        assert_eq!(result, None);
        assert_eq!(out, OSC11_QUERY, "the query should still have been sent");
    }

    #[test]
    fn a_replying_terminal_is_classified() {
        let mut out = Vec::new();
        let mut input = io::Cursor::new(b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\".to_vec());
        let result = query_background(&mut out, &mut input, Duration::from_millis(50)).unwrap();
        assert_eq!(result, Some(Background::Light));
    }

    #[test]
    fn a_profile_without_a_query_falls_back_and_says_so() {
        let p = TerminalProfile::from_env(&env(&[("TERM", "xterm-256color")]), None);
        assert_eq!(p.background, Background::FALLBACK);
        assert_eq!(p.background_source, BackgroundSource::Fallback);
        assert_eq!(p.capabilities.color_depth, ColorDepth::Ansi256);
        // Windows consoles are all ConPTY-backed and take the newline path.
        let expected = if cfg!(windows) {
            InsertMode::FullScreen
        } else {
            InsertMode::ScrollRegion
        };
        assert_eq!(p.insert_mode, expected);
    }

    #[test]
    fn a_user_setting_outranks_any_query() {
        let p =
            TerminalProfile::from_env(&env(&[("TERM", "xterm-256color")]), Some(Background::Light));
        assert_eq!(p.background, Background::Light);
        assert_eq!(p.background_source, BackgroundSource::UserSetting);
    }

    #[test]
    fn a_multiplexer_selects_the_safe_insert_mode() {
        let p = TerminalProfile::from_env(&env(&[("TERM", "xterm"), ("TMUX", "/s")]), None);
        assert_eq!(p.insert_mode, InsertMode::MultiplexerSafe);
        assert_eq!(p.capabilities.image_protocol, ImageProtocol::None);
    }

    // ── S:I4 OSC52 copy fallback ────────────────────────────────────────
    #[test]
    fn copy_via_osc52_emits_plain_sequence_for_plain_env() {
        let env = env(&[("TERM", "xterm-256color")]);
        let mut out = Vec::new();
        let n = copy_via_osc52(&mut out, "hi", &env).unwrap();
        assert!(n > 0);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1b]52;c;"));
        assert!(s.ends_with("\x07"));
    }

    #[test]
    fn copy_via_osc52_wraps_for_tmux_env() {
        let env = env(&[("TMUX", "/tmp/tmux"), ("TERM", "screen")]);
        let mut out = Vec::new();
        copy_via_osc52(&mut out, "tmux-copy", &env).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1bPtmux;"), "tmux must wrap, got {s:?}");
    }

    #[test]
    fn copy_via_osc52_wraps_for_ssh_inside_tmux() {
        let env = env(&[
            ("SSH_CONNECTION", "1 2 3 4"),
            ("TMUX", "/tmp/tmux"),
            ("TERM", "tmux-256color"),
        ]);
        let mut out = Vec::new();
        copy_via_osc52(&mut out, "ssh-copy", &env).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1bPtmux;"));
    }

    #[test]
    fn copy_via_osc52_rejects_oversized_payload() {
        let env = env(&[("TERM", "xterm-256color")]);
        let mut out = Vec::new();
        let error = copy_via_osc52(&mut out, &"x".repeat(100_001), &env).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(out.is_empty());
    }
}
