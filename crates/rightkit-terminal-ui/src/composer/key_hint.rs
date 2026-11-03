//! Key binding primitives and input matching for the TUI.
//!
//! This module provides `KeyBinding`, the runtime representation of a single
//! keybinding (key code + modifier set), along with matching logic that handles
//! cross-terminal inconsistencies in how shifted letters and raw C0 control
//! characters are reported.
//!
//! List and picker code should match navigation through these helpers instead
//! of comparing `KeyEvent` values directly. The matcher owns compatibility for
//! terminals that report control chords as C0 characters, while
//! `is_plain_text_key_event` gives searchable pickers a shared boundary between
//! text input and navigation commands.
//!
//! Rendering helpers emphasize each complete shortcut uniformly, including its separators.
//! Plain-text uses share the same compact labels.

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::Span;

#[cfg(target_os = "macos")]
const MODIFIER_LABELS: [(KeyModifiers, &str); 3] = [
    (KeyModifiers::CONTROL, "⌃"),
    (KeyModifiers::SHIFT, "⇧"),
    (KeyModifiers::ALT, "⌥"),
];
#[cfg(not(target_os = "macos"))]
const MODIFIER_LABELS: [(KeyModifiers, &str); 3] = [
    #[cfg(target_os = "linux")]
    (KeyModifiers::CONTROL, "^"),
    #[cfg(not(target_os = "linux"))]
    (KeyModifiers::CONTROL, "ctrl"),
    (KeyModifiers::SHIFT, "shift"),
    (KeyModifiers::ALT, "alt"),
];

/// One concrete key event that can trigger a TUI action.
///
/// Matching via `is_press` handles exact equality plus compatibility fallbacks
/// for terminals that report uppercase letters without SHIFT and Ctrl keys as
/// raw C0 control characters. This means a binding defined as `shift-a` will
/// match either `Shift+a` or plain `A`, and `ctrl-j` will match raw LF.
///
/// This does not model multi-key chords or partial matches; callers that need
/// sequences must keep that state outside this type.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct KeyBinding {
    key: KeyCode,
    modifiers: KeyModifiers,
}

/// A user-visible shortcut made from one key or a two-key chord.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ShortcutHint {
    Single(KeyBinding),
}

impl ShortcutHint {
    pub(crate) fn display_label(self) -> String {
        match self {
            Self::Single(binding) => binding.display_label(),
        }
    }
}

impl From<KeyBinding> for ShortcutHint {
    fn from(binding: KeyBinding) -> Self {
        Self::Single(binding)
    }
}

impl KeyBinding {
    pub(crate) fn display_label(&self) -> String {
        let mut label = String::new();
        for (modifier, name) in MODIFIER_LABELS {
            if self.modifiers.contains(modifier) {
                label.push_str(name);
                if !matches!(name, "⌃" | "⇧" | "⌥" | "⌘" | "^") {
                    label.push('+');
                }
            }
        }
        let key = match self.key {
            KeyCode::Enter => "enter".to_string(),
            KeyCode::Char(' ') => "space".to_string(),
            KeyCode::Up => "↑".to_string(),
            KeyCode::Down => "↓".to_string(),
            KeyCode::Left => "←".to_string(),
            KeyCode::Right => "→".to_string(),
            KeyCode::PageUp => "pgup".to_string(),
            KeyCode::PageDown => "pgdn".to_string(),
            _ => self.key.to_string().to_ascii_lowercase(),
        };
        label.push_str(&key);
        label
    }

    pub(crate) const fn new(key: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { key, modifiers }
    }

    pub fn is_press(&self, event: KeyEvent) -> bool {
        normalize_key_parts(self.key, self.modifiers)
            == normalize_key_parts(event.code, event.modifiers)
            && (event.kind == KeyEventKind::Press || event.kind == KeyEventKind::Repeat)
    }

    pub(crate) const fn parts(&self) -> (KeyCode, KeyModifiers) {
        (self.key, self.modifiers)
    }

    /// Legacy Ctrl+] is reported as Ctrl+5; keep matching and conflicts consistent.
    pub(crate) fn normalized_parts(&self) -> (KeyCode, KeyModifiers) {
        match self.parts() {
            (KeyCode::Char('5'), KeyModifiers::CONTROL) => ctrl(KeyCode::Char(']')).parts(),
            parts => parts,
        }
    }
}

pub(crate) fn normalize_key_parts(
    key: KeyCode,
    mut modifiers: KeyModifiers,
) -> (KeyCode, KeyModifiers) {
    let KeyCode::Char(ch) = key else {
        return (key, modifiers);
    };
    if modifiers.is_empty()
        && let Some(ctrl_char) = c0_control_char_to_ctrl_char(ch)
    {
        return normalize_key_parts(KeyCode::Char(ctrl_char), KeyModifiers::CONTROL);
    }
    if ch.is_ascii_uppercase() {
        modifiers.insert(KeyModifiers::SHIFT);
        return (KeyCode::Char(ch.to_ascii_lowercase()), modifiers);
    }
    KeyBinding::new(key, modifiers).normalized_parts()
}

fn c0_control_char_to_ctrl_char(ch: char) -> Option<char> {
    let code = u32::from(ch);
    match code {
        0x00 => Some(' '),
        0x01..=0x1a => char::from_u32(code - 0x01 + u32::from('a')),
        0x1c..=0x1f => char::from_u32(code - 0x1c + u32::from('4')),
        _ => None,
    }
}

/// Matching helpers for one action's keybinding set.
///
/// Implementations are expected to treat the slice as alternatives for one
/// action. They should not interpret order as priority for dispatch; order is
/// reserved for UI hint selection via `primary_binding`.
pub(crate) trait KeyBindingListExt {
    /// True when any binding in this set matches `event`.
    fn is_pressed(&self, event: KeyEvent) -> bool;
}

impl KeyBindingListExt for [KeyBinding] {
    fn is_pressed(&self, event: KeyEvent) -> bool {
        self.iter().any(|binding| binding.is_press(event))
    }
}

pub(crate) const fn plain(key: KeyCode) -> KeyBinding {
    KeyBinding::new(key, KeyModifiers::NONE)
}

pub(crate) const fn alt(key: KeyCode) -> KeyBinding {
    KeyBinding::new(key, KeyModifiers::ALT)
}

pub(crate) const fn shift(key: KeyCode) -> KeyBinding {
    KeyBinding::new(key, KeyModifiers::SHIFT)
}

pub(crate) const fn ctrl(key: KeyCode) -> KeyBinding {
    KeyBinding::new(key, KeyModifiers::CONTROL)
}

impl From<KeyBinding> for Span<'static> {
    fn from(binding: KeyBinding) -> Self {
        (&binding).into()
    }
}
impl From<&KeyBinding> for Span<'static> {
    fn from(binding: &KeyBinding) -> Self {
        Span::styled(binding.display_label(), key_hint_style())
    }
}

impl From<ShortcutHint> for Span<'static> {
    fn from(hint: ShortcutHint) -> Self {
        Span::styled(hint.display_label(), key_hint_style())
    }
}

fn key_hint_style() -> Style {
    // Donor resolves this through the detected terminal palette; the composer
    // seam uses a palette-independent hint style until the parent's theme
    // bridge lands.
    let _ = Color::Reset;
    Style::default().bold().not_dim()
}

pub(crate) fn has_ctrl_or_alt(mods: KeyModifiers) -> bool {
    (mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT)) && !is_altgr(mods)
}

#[cfg(windows)]
#[inline]
pub(crate) fn is_altgr(mods: KeyModifiers) -> bool {
    mods.contains(KeyModifiers::ALT) && mods.contains(KeyModifiers::CONTROL)
}

#[cfg(not(windows))]
#[inline]
pub(crate) fn is_altgr(_mods: KeyModifiers) -> bool {
    false
}
