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
pub mod kitty_image;
pub mod line_utils;
mod link_pointer;
pub mod notify;
pub mod scrollback;
/// Session modes and restoration, ported from the donor `tui.rs` lifecycle.
/// The host owns when to call `set_modes`/`restore`; this module owns the
/// exact escape discipline.
pub mod session;
mod tmux;
#[cfg(windows)]
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

use std::io::{self, Write};
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

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
/// that a silent terminal does not visibly delay startup: the question is
/// asked once on the launch path, before the key reader starts, so this is the
/// whole delay a silent terminal costs.
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
    if hex.len() != 6 || !hex.is_ascii() {
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

/// Whether stdin was already in raw (non-canonical) mode when this process
/// got it: a terminal inherited raw from a parent that is still reading it.
/// Call it before entering raw mode. [`query_background`] must not run then.
pub fn stdin_inherited_raw() -> bool {
    #[cfg(unix)]
    {
        let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: `tcgetattr` only writes the struct it is handed, and the
        // struct is read only after the call reported success.
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, termios.as_mut_ptr()) } != 0 {
            return false;
        }
        // SAFETY: initialised by the successful `tcgetattr` above.
        let termios = unsafe { termios.assume_init() };
        termios.c_lflag & libc::ICANON == 0
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// The longest reply body (the text after `11;`) taken for an OSC 11 answer.
const OSC11_REPLY_MAX: usize = 64;

/// Recognises a terminal's OSC 11 reply in the decoded key stream and keeps
/// it out of the editor.
///
/// Crossterm has no OSC parser: `ESC ] 11 ; <colour> BEL` reaches the reader
/// as Alt+`]`, the characters `1`, `1`, `;` and the colour text, then Ctrl+G
/// (or Alt+`\` for an `ESC \` terminator). Unfiltered that is typed text, and
/// a Ctrl+G that opens the external editor. While a reply is owed, Alt+`]` is
/// held back until the next keys show whether it starts a reply; the caller
/// polls with a short timeout while [`Self::holding`] and calls
/// [`Self::release`] when nothing followed. A reply arrives as one burst, a
/// person cannot type `ESC ] 1 1 ;` inside that window. One query earns one
/// reply, so the filter switches itself off after swallowing it.
#[derive(Debug, Default)]
pub struct Osc11ReplyFilter {
    armed: bool,
    /// Keys of a possible reply, held until it is one or is not.
    held: Vec<Event>,
    /// The reply text after `11;`, once that introducer has matched.
    body: Option<String>,
}

impl Osc11ReplyFilter {
    /// A filter that passes everything: no query was sent, or its reply came.
    pub fn off() -> Self {
        Self::default()
    }

    /// A filter for a query whose reply has not arrived yet.
    pub fn armed() -> Self {
        Self {
            armed: true,
            ..Self::default()
        }
    }

    /// Whether keys are held back waiting for the rest of a possible reply.
    pub fn holding(&self) -> bool {
        !self.held.is_empty()
    }

    /// Take one decoded event. Events that are not part of a reply are
    /// appended to `pass` in order; a complete reply is swallowed and its
    /// body (the text between `11;` and the terminator) returned.
    pub fn feed(&mut self, event: Event, pass: &mut Vec<Event>) -> Option<String> {
        let key = match &event {
            Event::Key(key) if self.armed && key.kind == KeyEventKind::Press => *key,
            // Releases, resizes, focus and mouse reports are never reply text.
            _ => {
                pass.push(event);
                return None;
            }
        };
        if self.held.is_empty() {
            if key.code == KeyCode::Char(']') && key.modifiers == KeyModifiers::ALT {
                self.held.push(event);
            } else {
                pass.push(event);
            }
            return None;
        }
        let plain = (key.modifiers - KeyModifiers::SHIFT).is_empty();
        let Some(body) = self.body.as_mut() else {
            // Matching the introducer `11;` after the held Alt+`]`.
            let expected = ['1', '1', ';'][self.held.len() - 1];
            if key.code == KeyCode::Char(expected) && plain {
                self.held.push(event);
                if self.held.len() == 4 {
                    self.body = Some(String::new());
                }
                return None;
            }
            self.release(pass);
            return self.feed(event, pass);
        };
        match key.code {
            KeyCode::Char('g') if key.modifiers == KeyModifiers::CONTROL => {}
            KeyCode::Char('\\') if key.modifiers == KeyModifiers::ALT => {}
            KeyCode::Char(c) if plain && body.chars().count() < OSC11_REPLY_MAX => {
                body.push(c);
                self.held.push(event);
                return None;
            }
            _ => {
                // The introducer matched, so what is held is terminal output,
                // never typed text: drop it, keep the key that ended it.
                self.release(pass);
                pass.push(event);
                return None;
            }
        }
        self.held.clear();
        self.armed = false;
        self.body.take()
    }

    /// Nothing followed the held keys in time. A lone Alt+`]` (or a prefix
    /// that is not a reply yet) was typed and goes to `pass`; a reply cut off
    /// after its introducer is terminal output and is dropped.
    pub fn release(&mut self, pass: &mut Vec<Event>) {
        if self.body.take().is_some() {
            self.held.clear();
        } else {
            pass.append(&mut self.held);
        }
    }
}

/// What came of asking the terminal for its background colour.
#[derive(Debug, Default)]
pub struct BackgroundQuery {
    /// The answer, when a well-formed reply arrived in time.
    pub background: Option<Background>,
    /// What the person typed while the question was open, in order. The
    /// caller hands these to its input path so type-ahead is not lost.
    pub typed: Vec<Event>,
    /// The recogniser for a reply that is still owed (the terminal was silent
    /// or slow). The input path must run every event through it so a late
    /// reply never reaches the editor as typed text.
    pub late_reply: Osc11ReplyFilter,
}

/// Ask the terminal for its background colour.
///
/// Waits at most `timeout` ([`BACKGROUND_QUERY_TIMEOUT`]) and returns as soon
/// as a reply is complete, well-formed or not. The reply is read through
/// crossterm's own event source, so nothing else typed is consumed and no
/// bytes are taken from under its parser. The caller must already have the
/// terminal in raw mode, or the reply will be line-buffered and arrive too
/// late, and must not have a key reader running yet: it would race this read.
///
/// Never call this on a terminal inherited in raw mode from a parent process
/// (a resumed session): the reply races whatever the parent was reading. See
/// [`stdin_inherited_raw`].
pub fn query_background(out: &mut impl Write, timeout: Duration) -> io::Result<BackgroundQuery> {
    out.write_all(OSC11_QUERY)?;
    out.flush()?;

    let mut query = BackgroundQuery {
        late_reply: Osc11ReplyFilter::armed(),
        ..BackgroundQuery::default()
    };
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        // A failing event source ends the wait; the reply may still come, so
        // the filter stays armed for the input path.
        if remaining.is_zero() || !matches!(crossterm::event::poll(remaining), Ok(true)) {
            break;
        }
        let Ok(event) = crossterm::event::read() else {
            break;
        };
        if let Some(body) = query.late_reply.feed(event, &mut query.typed) {
            query.background = parse_osc11(&format!("\x1b]11;{body}\x07")).map(classify);
            break;
        }
    }
    Ok(query)
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
