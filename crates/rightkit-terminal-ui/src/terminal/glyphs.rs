//! ASCII fallback for terminals detected without Unicode.
//!
//! The UI is drawn with a small fixed vocabulary of symbols (`✓ ✗ ⇥ ↳ ┆ ▌ ●`
//! ...). On a Unicode terminal they are emitted untouched. On a terminal
//! detected as non-Unicode (see [`super::capability::detect_unicode`]) every
//! symbol maps to ONE ASCII column, so layout arithmetic done on the Unicode
//! text stays exact.

use std::borrow::Cow;
use std::sync::atomic::{AtomicBool, Ordering};

static ASCII_ONLY: AtomicBool = AtomicBool::new(false);

/// Adopt the detected capability for this process.
pub fn set_ascii_only(ascii_only: bool) {
    ASCII_ONLY.store(ascii_only, Ordering::Relaxed);
}

pub fn ascii_only() -> bool {
    ASCII_ONLY.load(Ordering::Relaxed)
}

/// One-column ASCII stand-in for a symbol the UI draws, or `None` to keep it.
pub fn ascii_for(ch: char) -> Option<char> {
    Some(match ch {
        '✓' => '+',
        '✗' | '✕' => 'x',
        '⇥' | '↳' | '›' | '▸' => '>',
        '┆' | '┊' => ':',
        '▌' | '┃' | '│' | '▏' => '|',
        '●' | '•' | '✻' => '*',
        '○' => 'o',
        '↻' => '~',
        '─' | '━' | '╌' => '-',
        '╰' | '╯' | '└' => '`',
        '↑' => '^',
        '↓' => 'v',
        '⏎' => '<',
        '…' => '.',
        '·' => '.',
        '—' | '–' => '-',
        '→' => '>',
        '←' => '<',
        _ => return None,
    })
}

/// `text` as it should reach the terminal.
pub fn for_terminal(text: &str) -> Cow<'_, str> {
    if !ascii_only() || text.is_ascii() {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.chars().map(|ch| ascii_for(ch).unwrap_or(ch)).collect())
}
