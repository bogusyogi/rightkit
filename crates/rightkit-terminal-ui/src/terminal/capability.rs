//! Terminal capability detection.
//!
//! Every probe here degrades; none of them fail or block startup. A terminal
//! that answers nothing still gets a working, legible UI.
//!
//! Colour depth and image support are decided from the environment alone --
//! no surveyed harness queries the terminal for either, and a query costs a
//! round trip we would pay on every launch. Background lightness *is* worth a
//! query (OSC 11), but that lives in the parent module because it needs I/O;
//! everything here stays pure so it can be tested exhaustively.

use std::collections::BTreeMap;

/// How many colours the terminal can render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColorDepth {
    /// No colour at all: `TERM=dumb` or an explicit `NO_COLOR`.
    Monochrome,
    /// The 8 base colours plus their bright variants.
    Ansi16,
    /// The xterm 256-colour palette: 16 system + 6x6x6 cube + 24 greys.
    Ansi256,
    /// 24-bit direct colour.
    TrueColor,
}

/// Which inline-image protocol the terminal speaks, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProtocol {
    /// Kitty graphics protocol. Kitty, Ghostty, WezTerm.
    Kitty,
    /// iTerm2 inline images (OSC 1337).
    ITerm2,
    /// No inline image support. Render a textual placeholder.
    None,
}

/// Everything detected about the host terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub color_depth: ColorDepth,
    pub image_protocol: ImageProtocol,
    /// True when a terminal multiplexer sits between us and the real terminal.
    /// Multiplexers strip graphics escapes and emulate scroll regions
    /// imperfectly, so several features must be disabled or routed differently.
    pub multiplexer: Option<Multiplexer>,
    /// True when the host is Windows Terminal. WT can drop rows scrolled out
    /// of a partial DECSTBM region instead of pushing them into scrollback,
    /// so history insertion and viewport growth take the newline-scrolling
    /// [`super::InsertMode::FullScreen`] path there.
    pub windows_terminal: bool,
    /// False when the terminal is detected without Unicode: the UI then falls
    /// back to one-column ASCII stand-ins ([`super::glyphs`]).
    pub unicode: bool,
}

/// A multiplexer between us and the real terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Multiplexer {
    Tmux,
    Screen,
    /// Zellij needs an entirely different history-insertion strategy: its
    /// scroll-region emulation does not confine soft-wrap continuation rows to
    /// the requested region.
    Zellij,
}

/// Environment lookup, injectable so detection can be tested without touching
/// the real process environment.
pub trait Env {
    fn get(&self, key: &str) -> Option<String>;
}

/// The real process environment.
pub struct SystemEnv;

impl Env for SystemEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

impl Env for BTreeMap<String, String> {
    fn get(&self, key: &str) -> Option<String> {
        BTreeMap::get(self, key).cloned()
    }
}

/// Terminals whose GPU glyph cache misrenders truecolour spans, so we step them
/// down to 256 even when they advertise otherwise. Matched against
/// `TERM_PROGRAM`, lowercased.
const FRAGILE_GLYPH_CACHE: &[&str] = &["vscode", "apple_terminal"];

/// `TERM_PROGRAM` values known to handle 24-bit colour correctly.
const TRUECOLOR_PROGRAMS: &[&str] = &[
    "ghostty",
    "iterm.app",
    "wezterm",
    "warpterminal",
    "alacritty",
    "hyper",
];

/// Vendor-specific env vars that imply a truecolour-capable terminal even when
/// `COLORTERM` is unset -- common when the shell is launched by a GUI app that
/// does not export it.
const TRUECOLOR_MARKERS: &[&str] = &["GHOSTTY_RESOURCES_DIR", "WEZTERM_PANE", "KITTY_WINDOW_ID"];

/// Whether the terminal can show the UI's Unicode symbols. Conservative:
/// Unicode unless the environment says otherwise. `RIGHTKIT_ASCII` forces the
/// fallback; `RIGHTKIT_UNICODE` forces Unicode; legacy `TERM` values
/// (`linux`, `dumb`, `vt100`, `vt220`, `ansi`) and a locale that names a
/// non-UTF-8 charset opt out. Windows consoles are Unicode-capable.
pub fn detect_unicode(env: &impl Env) -> bool {
    if env
        .get("RIGHTKIT_ASCII")
        .is_some_and(|v| !v.is_empty() && v != "0")
    {
        return false;
    }
    if env
        .get("RIGHTKIT_UNICODE")
        .is_some_and(|v| !v.is_empty() && v != "0")
    {
        return true;
    }
    if let Some(term) = env.get("TERM") {
        if matches!(term.as_str(), "linux" | "dumb" | "vt100" | "vt220" | "ansi") {
            return false;
        }
    }
    // The first locale variable that is set decides (LC_ALL > LC_CTYPE > LANG).
    for key in ["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Some(value) = env.get(key).filter(|v| !v.is_empty()) {
            let lower = value.to_ascii_lowercase();
            if lower == "c" || lower == "posix" {
                return false;
            }
            return !lower.contains('.') || lower.contains("utf-8") || lower.contains("utf8");
        }
    }
    true
}

/// Detect every capability we decide from the environment.
pub fn detect(env: &impl Env) -> Capabilities {
    let multiplexer = detect_multiplexer(env);
    Capabilities {
        color_depth: detect_color_depth(env),
        image_protocol: detect_image_protocol(env, multiplexer),
        multiplexer,
        windows_terminal: detect_windows_terminal(env),
        unicode: detect_unicode(env),
    }
}

/// Resolve colour depth from the environment.
///
/// Order matters: an explicit opt-out wins over everything, then a terminal
/// known to render truecolour badly is stepped down regardless of what it
/// claims, then positive signals are tried cheapest-first.
pub fn detect_color_depth(env: &impl Env) -> ColorDepth {
    // Honoured by convention across CLI tooling: presence alone means "no
    // colour", whatever the value.
    if env.get("NO_COLOR").is_some() {
        return ColorDepth::Monochrome;
    }

    let term = env.get("TERM").unwrap_or_default().to_ascii_lowercase();
    if term.is_empty() || term == "dumb" {
        return ColorDepth::Monochrome;
    }

    let term_program = env
        .get("TERM_PROGRAM")
        .unwrap_or_default()
        .to_ascii_lowercase();

    // Step down before stepping up: these terminals advertise truecolour and
    // then corrupt the glyph cache rendering it.
    let fragile = FRAGILE_GLYPH_CACHE.contains(&term_program.as_str());

    let colorterm = env
        .get("COLORTERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let claims_truecolor = colorterm == "truecolor"
        || colorterm == "24bit"
        || TRUECOLOR_PROGRAMS.contains(&term_program.as_str())
        || TRUECOLOR_MARKERS.iter().any(|k| env.get(k).is_some())
        || term.contains("kitty")
        || term.contains("ghostty")
        || term.contains("alacritty")
        || term.contains("direct");

    if claims_truecolor {
        return if fragile {
            ColorDepth::Ansi256
        } else {
            ColorDepth::TrueColor
        };
    }

    if term.contains("256") {
        return ColorDepth::Ansi256;
    }

    // A named terminal that got this far still supports the base palette.
    ColorDepth::Ansi16
}

/// Detect a multiplexer sitting between us and the terminal.
pub fn detect_multiplexer(env: &impl Env) -> Option<Multiplexer> {
    if env.get("ZELLIJ").is_some()
        || env.get("ZELLIJ_SESSION_NAME").is_some()
        || env.get("ZELLIJ_VERSION").is_some()
    {
        return Some(Multiplexer::Zellij);
    }
    if env.get("TMUX").is_some() || env.get("TMUX_PANE").is_some() {
        return Some(Multiplexer::Tmux);
    }
    let term = env.get("TERM").unwrap_or_default().to_ascii_lowercase();
    if term.starts_with("tmux") {
        return Some(Multiplexer::Tmux);
    }
    if term.starts_with("screen") {
        return Some(Multiplexer::Screen);
    }
    None
}

// ── donor terminal-detection (codex-rs/terminal-detection/src/lib.rs) ─────
//
// Ported at the same revision (`444da310`, Apache-2.0), adapted to read
// through our `Env` trait instead of the donor's private `Environment`
// trait. `Screen` has no donor analogue; it folds into `Unknown`-name
// detection while keeping `Multiplexer::Screen` for insertion policy.

/// Known terminal name categories derived from environment variables.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalName {
    /// Apple Terminal (Terminal.app).
    AppleTerminal,
    /// Ghostty terminal emulator.
    Ghostty,
    /// iTerm2 terminal emulator.
    Iterm2,
    /// Warp terminal emulator.
    WarpTerminal,
    /// Visual Studio Code integrated terminal.
    VsCode,
    /// WezTerm terminal emulator.
    WezTerm,
    /// kitty terminal emulator.
    Kitty,
    /// Alacritty terminal emulator.
    Alacritty,
    /// KDE Konsole terminal emulator.
    Konsole,
    /// GNOME Terminal emulator.
    GnomeTerminal,
    /// VTE backend terminal.
    Vte,
    /// Windows Terminal emulator.
    WindowsTerminal,
    /// Dumb terminal (TERM=dumb).
    Dumb,
    /// Unknown or missing terminal identification.
    Unknown,
}

/// Structured terminal identification data (donor `TerminalInfo`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalInfo {
    /// The detected terminal name category.
    pub name: TerminalName,
    /// The `TERM_PROGRAM` value when provided by the terminal.
    pub term_program: Option<String>,
    /// The terminal version string when available.
    pub version: Option<String>,
    /// The `TERM` value when falling back to capability strings.
    pub term: Option<String>,
    /// Multiplexer metadata when a terminal multiplexer is active.
    pub multiplexer: Option<Multiplexer>,
}

impl TerminalInfo {
    /// Returns whether the active terminal multiplexer is Zellij.
    pub fn is_zellij(&self) -> bool {
        matches!(self.multiplexer, Some(Multiplexer::Zellij))
    }
}

/// Structured terminal metadata for the current process, computed once.
pub fn terminal_info() -> TerminalInfo {
    static TERMINAL_INFO: std::sync::OnceLock<TerminalInfo> = std::sync::OnceLock::new();
    TERMINAL_INFO
        .get_or_init(|| detect_terminal_info(&SystemEnv))
        .clone()
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

/// Donor `detect_terminal_info_from_env`, adapted to our `Env` trait.
pub fn detect_terminal_info(env: &impl Env) -> TerminalInfo {
    let multiplexer = detect_multiplexer(env);

    if let Some(term_program) = non_empty(env.get("TERM_PROGRAM"))
        && !term_program.eq_ignore_ascii_case("tmux")
    {
        let version = non_empty(env.get("TERM_PROGRAM_VERSION"));
        let name = terminal_name_from_term_program(&term_program).unwrap_or(TerminalName::Unknown);
        return TerminalInfo {
            name,
            term_program: Some(term_program),
            version,
            term: None,
            multiplexer,
        };
    }

    let named = |name: TerminalName, version: Option<String>| TerminalInfo {
        name,
        term_program: None,
        version,
        term: None,
        multiplexer: multiplexer.clone(),
    };

    if non_empty(env.get("GHOSTTY_RESOURCES_DIR")).is_some() {
        return named(TerminalName::Ghostty, None);
    }
    if env.get("WEZTERM_VERSION").is_some() {
        return named(TerminalName::WezTerm, non_empty(env.get("WEZTERM_VERSION")));
    }
    if env.get("ITERM_SESSION_ID").is_some()
        || env.get("ITERM_PROFILE").is_some()
        || env.get("ITERM_PROFILE_NAME").is_some()
    {
        return named(TerminalName::Iterm2, None);
    }
    if env.get("TERM_SESSION_ID").is_some() {
        return named(TerminalName::AppleTerminal, None);
    }
    if env.get("KITTY_WINDOW_ID").is_some()
        || env.get("TERM").is_some_and(|term| term.contains("kitty"))
    {
        return named(TerminalName::Kitty, None);
    }
    if env.get("ALACRITTY_SOCKET").is_some()
        || env.get("TERM").is_some_and(|term| term == "alacritty")
    {
        return named(TerminalName::Alacritty, None);
    }
    if env.get("KONSOLE_VERSION").is_some() {
        return named(TerminalName::Konsole, non_empty(env.get("KONSOLE_VERSION")));
    }
    if env.get("GNOME_TERMINAL_SCREEN").is_some() {
        return named(TerminalName::GnomeTerminal, None);
    }
    if env.get("VTE_VERSION").is_some() {
        return named(TerminalName::Vte, non_empty(env.get("VTE_VERSION")));
    }
    if env.get("WT_SESSION").is_some() {
        return named(TerminalName::WindowsTerminal, None);
    }

    if let Some(term) = non_empty(env.get("TERM")) {
        let name = match term.as_str() {
            "dumb" => TerminalName::Dumb,
            "xterm-ghostty" => TerminalName::Ghostty,
            "wezterm" | "wezterm-mux" => TerminalName::WezTerm,
            _ => TerminalName::Unknown,
        };
        return TerminalInfo {
            name,
            term_program: None,
            version: None,
            term: Some(term),
            multiplexer,
        };
    }

    TerminalInfo {
        name: TerminalName::Unknown,
        term_program: None,
        version: None,
        term: None,
        multiplexer,
    }
}

fn terminal_name_from_term_program(value: &str) -> Option<TerminalName> {
    let normalized: String = value
        .trim()
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_' | '.'))
        .map(|c| c.to_ascii_lowercase())
        .collect();

    match normalized.as_str() {
        "appleterminal" => Some(TerminalName::AppleTerminal),
        "ghostty" => Some(TerminalName::Ghostty),
        "iterm" | "iterm2" | "itermapp" => Some(TerminalName::Iterm2),
        "warp" | "warpterminal" => Some(TerminalName::WarpTerminal),
        "vscode" => Some(TerminalName::VsCode),
        "wezterm" => Some(TerminalName::WezTerm),
        "kitty" => Some(TerminalName::Kitty),
        "alacritty" => Some(TerminalName::Alacritty),
        "konsole" => Some(TerminalName::Konsole),
        "gnometerminal" => Some(TerminalName::GnomeTerminal),
        "vte" => Some(TerminalName::Vte),
        "windowsterminal" => Some(TerminalName::WindowsTerminal),
        "dumb" => Some(TerminalName::Dumb),
        _ => None,
    }
}

/// Detect Windows Terminal.
///
/// Ported mechanism (OpenAI Codex `codex-rs/tui/src/tui/scrollback.rs`
/// `ScrollbackStrategy::detect` + `tui/windows_console.rs`, donor commit
/// 3074be908ac7f270b1b4781f2f8c55662067b96a, Apache-2.0): `WT_SESSION` is
/// set in every Windows Terminal tab and survives into WSL and SSH children
/// that keep the environment; `TERM_PROGRAM=WindowsTerminal` covers shells
/// that propagate `TERM_PROGRAM` instead.
pub fn detect_windows_terminal(env: &impl Env) -> bool {
    env.get("WT_SESSION").is_some()
        || env
            .get("TERM_PROGRAM")
            .is_some_and(|v| v.eq_ignore_ascii_case("WindowsTerminal"))
}

/// Decide which inline-image protocol to use.
///
/// A multiplexer is a hard rejection: tmux, screen, and Zellij all strip or
/// mangle graphics escapes, and emitting them anyway paints garbage into the
/// user's scrollback that they cannot clear.
pub fn detect_image_protocol(env: &impl Env, multiplexer: Option<Multiplexer>) -> ImageProtocol {
    if multiplexer.is_some() {
        return ImageProtocol::None;
    }

    if env.get("KITTY_WINDOW_ID").is_some()
        || env.get("GHOSTTY_RESOURCES_DIR").is_some()
        || env.get("WEZTERM_PANE").is_some()
        || env.get("WEZTERM_EXECUTABLE").is_some()
    {
        return ImageProtocol::Kitty;
    }

    let term_program = env
        .get("TERM_PROGRAM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    match term_program.as_str() {
        "ghostty" | "kitty" | "wezterm" => ImageProtocol::Kitty,
        "iterm.app" => ImageProtocol::ITerm2,
        _ => {
            if env.get("ITERM_SESSION_ID").is_some() {
                ImageProtocol::ITerm2
            } else {
                ImageProtocol::None
            }
        }
    }
}

// -- colour downsampling ---------------------------------------------------

/// The six levels each channel takes inside the xterm 6x6x6 colour cube.
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// Map a 24-bit colour onto the closest xterm-256 index.
///
/// Naive per-channel rounding into the cube is the obvious implementation and
/// it visibly misrenders: dark blues snap to black, and mid teals shift green.
/// So we generate both candidates -- the best cube cell and the best grey --
/// and pick whichever is perceptually closer.
pub fn rgb_to_xterm256(r: u8, g: u8, b: u8) -> u8 {
    let cube_index = |c: u8| -> usize {
        let mut best = 0usize;
        let mut best_distance = u32::MAX;
        for (i, level) in CUBE_LEVELS.iter().enumerate() {
            let d = (i32::from(c) - i32::from(*level)).unsigned_abs();
            if d < best_distance {
                best_distance = d;
                best = i;
            }
        }
        best
    };

    let (ri, gi, bi) = (cube_index(r), cube_index(g), cube_index(b));
    let cube_rgb = (CUBE_LEVELS[ri], CUBE_LEVELS[gi], CUBE_LEVELS[bi]);
    let cube_code = 16 + 36 * ri + 6 * gi + bi;
    let cube_distance = perceptual_distance((r, g, b), cube_rgb);

    // The 24-step grey ramp is finer than the cube's grey diagonal, so a
    // near-grey colour is usually better served by it.
    let grey_level = (u32::from(r) * 30 + u32::from(g) * 59 + u32::from(b) * 11) / 100;
    let grey_index = if grey_level < 8 {
        0
    } else if grey_level > 238 {
        23
    } else {
        ((grey_level - 8) / 10).min(23)
    };
    let grey_value = (8 + 10 * grey_index) as u8;
    let grey_code = 232 + grey_index as u8;
    let grey_distance = perceptual_distance((r, g, b), (grey_value, grey_value, grey_value));

    if grey_distance < cube_distance {
        grey_code
    } else {
        cube_code as u8
    }
}

/// Squared distance weighted for human sensitivity: green dominates perceived
/// brightness, blue contributes least.
fn perceptual_distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    let dr = (i32::from(a.0) - i32::from(b.0)).pow(2) as u32;
    let dg = (i32::from(a.1) - i32::from(b.1)).pow(2) as u32;
    let db = (i32::from(a.2) - i32::from(b.2)).pow(2) as u32;
    dr * 2 + dg * 4 + db
}

/// ── S:I4 clipboard OSC52 fallback chain (direct port of codex clipboard_copy.rs) ─
///
/// Codex uses terminal-mediated copy over SSH & native copy locally, with
/// terminal copy as fallback. We reproduce that decision tree without adding a new crate: when an SSH session is
/// detected we must use OSC52 (native clipboard is unreachable), when
/// inside tmux/screen we wrap OSC52 for the multiplexer passthrough, and
/// otherwise we attempt native first and fall back to OSC52. The caller
/// decides the write path; this module only classifies the environment
/// and builds the escape sequences so the logic is pure and testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardStrategy {
    /// Try native OS clipboard first (arboard), fall back to plain OSC52.
    NativeWithOsc52Fallback,
    /// SSH session detected – native is unreachable, use plain OSC52.
    Osc52,
    /// Inside tmux/screen – OSC52 must be wrapped for the multiplexer.
    Osc52Tmux,
}

/// Whether this environment is an SSH session (codex checks SSH_CONNECTION,
/// SSH_CLIENT and SSH_TTY).
pub fn is_ssh_session(env: &impl Env) -> bool {
    env.get("SSH_CONNECTION").is_some()
        || env.get("SSH_CLIENT").is_some()
        || env.get("SSH_TTY").is_some()
}

/// Classify the clipboard strategy for this environment, matching codex's
/// clipboard_copy.rs fallback order: remote terminal copy over SSH, otherwise
/// native clipboard first. Tmux passthrough remains part of terminal copy.
pub fn clipboard_strategy(env: &impl Env) -> ClipboardStrategy {
    if is_ssh_session(env) {
        return if detect_multiplexer(env).is_some() {
            ClipboardStrategy::Osc52Tmux
        } else {
            ClipboardStrategy::Osc52
        };
    }
    ClipboardStrategy::NativeWithOsc52Fallback
}

/// Build a plain OSC52 sequence for `text` (clipboard `c`).
pub fn osc52_sequence(text: &str) -> String {
    format!(
        "\x1b]52;c;{}\x07",
        rightkit_crypto::base64_encode(text.as_bytes())
    )
}

/// Build a tmux-wrapped OSC52 sequence. Codex wraps as
/// `\x1bPtmux;\x1b\x1b]52;c;<b64>\x07\x1b\\` so tmux forwards it.
pub fn osc52_tmux_sequence(text: &str) -> String {
    format!(
        "\x1bPtmux;\x1b\x1b]52;c;{}\x07\x1b\\",
        rightkit_crypto::base64_encode(text.as_bytes())
    )
}

/// Build the correct sequence for the current environment.
pub fn clipboard_osc52_for_env(text: &str, env: &impl Env) -> String {
    if detect_multiplexer(env).is_some() {
        osc52_tmux_sequence(text)
    } else {
        osc52_sequence(text)
    }
}

/// Map a 24-bit colour onto the 16-colour ANSI palette.
///
/// Only the coarse decision matters here: which of the eight hues, and whether
/// the bright variant reads closer.
pub fn rgb_to_ansi16(r: u8, g: u8, b: u8) -> u8 {
    let max = r.max(g).max(b);
    if max < 40 {
        return 0; // black
    }
    let bright = max > 170;
    let threshold = max / 2;
    let bit = |c: u8, shift: u8| -> u8 { u8::from(c > threshold) << shift };
    let base = bit(r, 0) | bit(g, 1) | bit(b, 2);
    if bright { base + 8 } else { base }
}

#[cfg(test)]
mod tests {

    fn env_of(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn unicode_is_assumed_unless_the_environment_says_otherwise() {
        assert!(detect_unicode(&env_of(&[])));
        assert!(detect_unicode(&env_of(&[("LANG", "en_US.UTF-8")])));
        assert!(detect_unicode(&env_of(&[
            ("LANG", "C.utf8"),
            ("TERM", "xterm-256color")
        ])));
        assert!(!detect_unicode(&env_of(&[("TERM", "linux")])));
        assert!(!detect_unicode(&env_of(&[("TERM", "dumb")])));
        assert!(!detect_unicode(&env_of(&[("LANG", "en_US.ISO-8859-1")])));
        assert!(!detect_unicode(&env_of(&[("LC_ALL", "C")])));
        // LC_ALL outranks LANG.
        assert!(detect_unicode(&env_of(&[
            ("LC_ALL", "en_US.UTF-8"),
            ("LANG", "C")
        ])));
        assert!(!detect_unicode(&env_of(&[
            ("RIGHTKIT_ASCII", "1"),
            ("LANG", "en_US.UTF-8")
        ])));
        assert!(detect_unicode(&env_of(&[
            ("RIGHTKIT_UNICODE", "1"),
            ("TERM", "linux")
        ])));
    }

    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn no_color_beats_every_positive_signal() {
        let e = env(&[
            ("NO_COLOR", "1"),
            ("COLORTERM", "truecolor"),
            ("TERM", "xterm-256color"),
        ]);
        assert_eq!(detect_color_depth(&e), ColorDepth::Monochrome);
    }

    #[test]
    fn dumb_and_empty_terminals_are_monochrome() {
        assert_eq!(
            detect_color_depth(&env(&[("TERM", "dumb")])),
            ColorDepth::Monochrome
        );
        assert_eq!(detect_color_depth(&env(&[])), ColorDepth::Monochrome);
    }

    #[test]
    fn colorterm_and_vendor_markers_both_imply_truecolor() {
        assert_eq!(
            detect_color_depth(&env(&[("TERM", "xterm"), ("COLORTERM", "truecolor")])),
            ColorDepth::TrueColor
        );
        assert_eq!(
            detect_color_depth(&env(&[("TERM", "xterm"), ("WEZTERM_PANE", "0")])),
            ColorDepth::TrueColor
        );
    }

    #[test]
    fn fragile_glyph_cache_terminals_are_stepped_down_despite_claiming_truecolor() {
        let e = env(&[
            ("TERM", "xterm-256color"),
            ("COLORTERM", "truecolor"),
            ("TERM_PROGRAM", "vscode"),
        ]);
        assert_eq!(detect_color_depth(&e), ColorDepth::Ansi256);
    }

    #[test]
    fn plain_256_and_bare_named_terminals_degrade_in_order() {
        assert_eq!(
            detect_color_depth(&env(&[("TERM", "xterm-256color")])),
            ColorDepth::Ansi256
        );
        assert_eq!(
            detect_color_depth(&env(&[("TERM", "xterm")])),
            ColorDepth::Ansi16
        );
    }

    #[test]
    fn multiplexers_are_detected_from_either_env_or_term() {
        assert_eq!(
            detect_multiplexer(&env(&[("ZELLIJ", "0")])),
            Some(Multiplexer::Zellij)
        );
        assert_eq!(
            detect_multiplexer(&env(&[("TMUX", "/tmp/sock")])),
            Some(Multiplexer::Tmux)
        );
        assert_eq!(
            detect_multiplexer(&env(&[("TERM", "screen-256color")])),
            Some(Multiplexer::Screen)
        );
        assert_eq!(detect_multiplexer(&env(&[("TERM", "xterm")])), None);
    }

    /// WT_SESSION is the reliable WT marker (Codex detection);
    /// TERM_PROGRAM is the fallback spelling.
    #[test]
    fn windows_terminal_is_detected_from_wt_session_or_term_program() {
        assert!(detect_windows_terminal(&env(&[("WT_SESSION", "guid")])));
        assert!(detect_windows_terminal(&env(&[(
            "TERM_PROGRAM",
            "WindowsTerminal"
        )])));
        assert!(!detect_windows_terminal(&env(&[("TERM", "xterm")])));
        // Detection lands on the profile-facing capability too.
        assert!(detect(&env(&[("WT_SESSION", "guid")])).windows_terminal);
        assert!(!detect(&env(&[("TERM", "xterm")])).windows_terminal);
    }

    #[test]
    fn a_multiplexer_disables_images_even_in_a_capable_terminal() {
        let e = env(&[("KITTY_WINDOW_ID", "1"), ("TMUX", "/tmp/sock")]);
        let mux = detect_multiplexer(&e);
        assert_eq!(detect_image_protocol(&e, mux), ImageProtocol::None);
    }

    #[test]
    fn image_protocol_picks_kitty_and_iterm_from_their_own_markers() {
        let kitty = env(&[("KITTY_WINDOW_ID", "1")]);
        assert_eq!(detect_image_protocol(&kitty, None), ImageProtocol::Kitty);

        let iterm = env(&[("TERM_PROGRAM", "iTerm.app")]);
        assert_eq!(detect_image_protocol(&iterm, None), ImageProtocol::ITerm2);

        let plain = env(&[("TERM", "xterm-256color")]);
        assert_eq!(detect_image_protocol(&plain, None), ImageProtocol::None);
    }

    #[test]
    fn cube_endpoints_map_to_their_exact_codes() {
        assert_eq!(rgb_to_xterm256(0, 0, 0), 16);
        assert_eq!(rgb_to_xterm256(255, 255, 255), 231);
        assert_eq!(rgb_to_xterm256(255, 0, 0), 196);
    }

    #[test]
    fn near_greys_prefer_the_grey_ramp_over_the_cube_diagonal() {
        // The cube's nearest diagonal entry to 0x77 is 0x87 (level 135), a
        // 16-step error; the grey ramp has an entry within 2.
        let code = rgb_to_xterm256(0x77, 0x77, 0x77);
        assert!(
            (232..=255).contains(&code),
            "expected a grey-ramp code, got {code}"
        );
    }

    #[test]
    fn dark_navy_does_not_collapse_to_black() {
        // The regression this whole function exists for: naive rounding sends
        // dark blues to the cube origin, losing the hue entirely.
        let code = rgb_to_xterm256(0x00, 0x00, 0x5f);
        assert_ne!(code, 16, "navy collapsed to black");
    }

    #[test]
    fn ansi16_separates_hue_and_brightness() {
        assert_eq!(rgb_to_ansi16(0, 0, 0), 0);
        assert_eq!(rgb_to_ansi16(255, 0, 0), 9); // bright red
        assert_eq!(rgb_to_ansi16(128, 0, 0), 1); // dim red
    }

    // ── S:I4 OSC52 fallback chain ───────────────────────────────────────
    #[test]
    fn ssh_session_uses_plain_osc52() {
        let e = env(&[("SSH_CONNECTION", "1.2 2.3 3.4 22"), ("TERM", "xterm")]);
        assert_eq!(clipboard_strategy(&e), ClipboardStrategy::Osc52);
        assert!(is_ssh_session(&e));
        let seq = clipboard_osc52_for_env("hi", &e);
        assert!(
            seq.starts_with("\x1b]52;c;"),
            "ssh must use plain OSC52, got {seq:?}"
        );
        assert!(!seq.contains("tmux"), "ssh must not wrap for tmux");
    }

    #[test]
    fn tmux_session_uses_wrapped_osc52() {
        let e = env(&[("TMUX", "/tmp/tmux-1000"), ("TERM", "screen-256color")]);
        assert_eq!(
            clipboard_strategy(&e),
            ClipboardStrategy::NativeWithOsc52Fallback
        );
        let seq = clipboard_osc52_for_env("hello", &e);
        assert!(seq.starts_with("\x1bPtmux;"), "tmux must wrap, got {seq:?}");
        assert!(seq.contains("]52;c;"), "wrapped must still carry OSC52");
    }

    #[test]
    fn plain_session_falls_back_to_native_with_osc52() {
        let e = env(&[("TERM", "xterm-256color")]);
        assert_eq!(
            clipboard_strategy(&e),
            ClipboardStrategy::NativeWithOsc52Fallback
        );
        let seq = clipboard_osc52_for_env("plain", &e);
        assert!(seq.starts_with("\x1b]52;c;"));
        assert!(!seq.contains("tmux"));
    }

    #[test]
    fn osc52_sequences_are_valid_base64_and_round_trip() {
        let cases = ["", "a", "hello", "hello world", "\u{1F600} emoji"];
        for text in cases {
            let seq = osc52_sequence(text);
            // Extract base64 payload between ";" and "\x07".
            let payload = seq
                .strip_prefix("\x1b]52;c;")
                .and_then(|s| s.strip_suffix("\x07"))
                .expect("plain OSC52 shape");
            // Re-decode manually to prove it matches input bytes.
            let decoded = {
                let mut out = Vec::new();
                let mut buf: u32 = 0;
                let mut bits: usize = 0;
                for c in payload.chars() {
                    if c == '=' {
                        break;
                    }
                    let val = match c {
                        'A'..='Z' => c as u32 - 'A' as u32,
                        'a'..='z' => c as u32 - 'a' as u32 + 26,
                        '0'..='9' => c as u32 - '0' as u32 + 52,
                        '+' => 62,
                        '/' => 63,
                        _ => panic!("unexpected b64 char {c}"),
                    };
                    buf = (buf << 6) | val;
                    bits += 6;
                    if bits >= 8 {
                        bits -= 8;
                        out.push(((buf >> bits) & 0xFF) as u8);
                    }
                }
                out
            };
            assert_eq!(decoded, text.as_bytes(), "round-trip failed for {text:?}");
        }
    }

    #[test]
    fn ssh_inside_tmux_uses_tmux_passthrough() {
        let e = env(&[
            ("SSH_CONNECTION", "1 2 3 4"),
            ("TMUX", "/tmp/tmux"),
            ("TERM", "tmux-256color"),
        ]);
        assert_eq!(clipboard_strategy(&e), ClipboardStrategy::Osc52Tmux);
    }
}
