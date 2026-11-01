use super::{catalog::*, matching::*};
use crate::command::CommandAction;
use std::collections::HashMap;

pub fn recognize_command(transcript: &str) -> Option<CommandAction> {
    if transcript.trim().is_empty() {
        return None;
    }
    let norm = normalize_for_match(transcript);
    if norm.is_empty() {
        return None;
    }
    if let Some((token, desc)) = direct_map().get(norm.as_str()) {
        return Some(token_to_action(token, desc));
    }
    if let Some(transform) = transform_map().get(norm.as_str()) {
        return Some(CommandAction::LastPasteTransform {
            transform: transform.to_string(),
        });
    }
    // Edit-distance fallback (fable #8): catch ASR mishears of a command phrase
    // ("undu" -> "undo") without firing on arbitrary speech. Exact match is
    // tried first above; this only runs on a miss, command-length input, with a
    // length-scaled budget and an unambiguous nearest phrase.
    if let Some((token, desc)) = fuzzy_direct_match(&norm) {
        return Some(token_to_action(token, desc));
    }
    if let Some(chord) = parse_chord(&norm) {
        let desc = format!("Chord {chord}");
        return Some(CommandAction::KeySequence {
            chords: vec![chord],
            description: Some(desc),
        });
    }
    // App launch ("open chrome") is resolved in the engine via a live scan of
    // installed apps (see `app_launch_query` + the host's app launcher),
    // because deciding command-vs-dictation requires knowing what's installed.
    None
}

/// How a Windows-authored command behaves on macOS. The catalog + chord tokens
/// are written Windows-first (`alt+tab`, `ctrl+c`, `windows+l`); this is the
/// single source of truth for translating them to macOS — consulted by BOTH the
/// dispatcher (what keys to send) and the catalog (which commands to show).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacCommand {
    /// Send the original chord through the semantic remap (Windows `ctrl` → ⌘),
    /// which is correct for editing/formatting/navigation chords.
    Default,
    /// Replace with this literal macOS chord. Tokens are literal here — `cmd`=⌘,
    /// `ctrl`=real ⌃ Control, `opt`/`alt`=⌥ Option, `shift`=⇧ — NOT remapped.
    Remap(&'static str),
    /// No macOS equivalent — hidden from the catalog and a no-op on dispatch.
    Unsupported,
}

/// Map a catalog action token (chord string or `__sentinel`) to its macOS
/// behavior. Pure + platform-neutral so it compiles everywhere; callers gate on
/// `cfg!(target_os = "macos")`.
pub fn macos_command(token: &str) -> MacCommand {
    use MacCommand::*;
    let t = token.trim().to_ascii_lowercase();
    // A comma sequence is supported iff every sub-chord is (dispatch splits these
    // into separate chords; this branch is for the catalog filter on the raw token).
    if t.contains(',') {
        return if t.split(',').all(|c| macos_command(c.trim()) != Unsupported) {
            Default
        } else {
            Unsupported
        };
    }
    match t.as_str() {
        // Window / app management — Windows chord → macOS equivalent.
        "alt+tab" => Remap("cmd+tab"),                  // Switch app
        "alt+shift+tab" => Remap("cmd+shift+tab"),      // Previous app
        "ctrl+tab" => Remap("ctrl+tab"),                // Next tab — REAL ⌃, not ⌘
        "ctrl+shift+tab" => Remap("ctrl+shift+tab"),    // Previous tab — REAL ⌃
        "alt+left" => Remap("cmd+["),                   // Back
        "alt+right" => Remap("cmd+]"),                  // Forward
        "alt+f4" => Remap("cmd+w"),                     // Close window
        "windows+down" => Remap("cmd+m"),               // Minimize
        "windows+l" => Unsupported,                     // macOS lock-screen shortcut is global/collision-prone; use no default mapping.
        "windows+shift+s" => Remap("cmd+shift+3"),      // Screenshot — whole screen (no area-drag; hands-free)
        "windows+printscreen" => Remap("cmd+shift+3"),  // Screenshot — same whole-screen capture
        "ctrl+shift+esc" => Unsupported,                // Windows Task Manager is not macOS Force Quit.
        "windows+tab" => Remap("ctrl+up"),              // Mission Control
        "ctrl+windows+right" => Remap("ctrl+right"),    // Next Space
        "ctrl+windows+left" => Remap("ctrl+left"),      // Previous Space
        "windows+r" | "windows+s" => Remap("cmd+space"),// Spotlight (~Run/Search)
        "windows+=" => Remap("cmd+opt+="),              // Zoom in (accessibility)
        "windows+-" => Remap("cmd+opt+-"),              // Zoom out
        "print screen" => Remap("cmd+shift+3"),         // Full screenshot
        // Editing — macOS word ops use ⌥, not ⌘ (so the ctrl→⌘ default is wrong).
        "ctrl+backspace" => Remap("opt+backspace"),     // Delete word back
        "ctrl+delete" => Remap("opt+delete"),           // Delete word forward
        "ctrl+shift+left" => Remap("opt+shift+left"),   // Select word
        // No macOS equivalent — hide + no-op (graceful, never hangs).
        "windows+up"                                    // Maximize
        | "windows+left" | "windows+right"              // Snap
        | "windows+shift+right" | "windows+shift+left"  // Move to monitor
        | "windows+d"                                   // Show desktop
        | "windows+e"                                   // File explorer (use "open finder")
        | "windows+i"                                   // Settings (use "open settings")
        | "windows+a"                                   // Action center
        | "windows+n"                                   // Notifications
        | "windows+v"                                   // Clipboard history
        | "windows+escape"                              // Close magnifier
        | "ctrl+windows+d"                              // New desktop
        | "left alt+left shift+printscreen"             // High contrast
        | "caps lock" | "num lock" | "scroll lock"      // Toggle keys (no mac equiv)
        | "insert"                                      // No Insert key on mac
        | "__always_on_top"
        | "__mac_force_quit"
        | "__power_hibernate"
        | "__power_cancel" => Unsupported,
        // Everything else: the ctrl→⌘ semantic remap is correct.
        _ => Default,
    }
}

/// Whether a direct-command action token should be considered available on the
/// current compiled platform. Unsupported platform commands should not merely
/// no-op at dispatch time: they should fail classification so dictation remains
/// dictation on that OS.
pub fn command_token_supported_on_current_platform(token: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        if token == "__mac_force_quit" || token == "__summarize_selection" {
            true
        } else {
            macos_command(token) != MacCommand::Unsupported
        }
    }
    #[cfg(target_os = "windows")]
    {
        token != "__mac_force_quit"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = token;
        false
    }
}

/// One discoverable group of voice commands for the "What can I say?" panel
/// (fable audit #4). Spoken `phrases` are aliases that produce the same effect,
/// grouped under a human `description` within a `category`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct CatalogEntry {
    pub category: String,
    pub description: String,
    pub phrases: Vec<String>,
}

/// Description allowlist for the Standalone commands panel. Every entry whose
/// description matches one of these shows in the "Standalone commands"
/// view — the panel scope is *hand actions* (switch window, new tab, find,
/// volume), NOT OS-shell admin verbs (Task Manager, Run dialog, Action
/// Center) and NOT inline formatting (those live in the FREE voice-commands
/// surface, not the Standalone one). The recognizer still matches every
/// DIRECT_COMMANDS row; this filter is presentation-only.
const STANDALONE_DESCRIPTIONS: &[&str] = &[
    // Selection / clipboard (work in any text field, free-feeling).
    "Select all",
    "Copy",
    "Cut",
    "Paste",
    "Undo",
    "Redo",
    "Backspace",
    "Delete word back",
    "Delete word forward",
    // Window / app switching.
    "Switch window",
    "Switch app",
    "Next tab",
    "Previous tab",
    "New tab",
    "Close tab",
    "Reopen tab",
    "Refresh",
    "Refresh page",
    "Reload page",
    "Back",
    "Forward",
    "Address bar",
    "Close window",
    "Minimize",
    "Maximize",
    "Snap left",
    "Snap right",
    "Show desktop",
    "Task view",
    // Find / zoom.
    "Find",
    "Find next",
    "Zoom in",
    "Zoom out",
    "Reset zoom",
    // Mouse / scroll.
    "Left click",
    "Right click",
    "Middle click",
    "Double click",
    "Triple click",
    "Scroll up",
    "Scroll down",
    "Scroll left",
    "Scroll right",
    "Page scroll up",
    "Page scroll down",
    // Volume / media — discrete hand actions, not app-internal.
    "Volume up",
    "Volume down",
    "Mute",
    "Play/Pause",
    "Next track",
    "Previous track",
    // macOS-specific.
    "Force Quit",
];

/// Build the user-facing command catalog from the same static tables the
/// recognizer uses — single source of truth, so the panel can never drift from
/// what actually fires. Aliases collapse under their shared description, in
/// first-seen order.
pub fn command_catalog() -> Vec<CatalogEntry> {
    fn grouped(
        category: &str,
        rows: impl Iterator<Item = (&'static str, &'static str)>,
    ) -> Vec<CatalogEntry> {
        // (description -> index) preserving first-seen order.
        let mut order: Vec<CatalogEntry> = Vec::new();
        let mut idx: HashMap<&'static str, usize> = HashMap::new();
        for (phrase, description) in rows {
            match idx.get(description) {
                Some(&i) => order[i].phrases.push(phrase.to_string()),
                None => {
                    idx.insert(description, order.len());
                    order.push(CatalogEntry {
                        category: category.to_string(),
                        description: description.to_string(),
                        phrases: vec![phrase.to_string()],
                    });
                }
            }
        }
        order
    }

    let mut out = Vec::new();

    // Control — end the recording (from COMMANDS_AND_LAYERS §A). These
    // in-stream tail triggers work on BOTH platforms: they're detected from the
    // transcript tail ("… zephyr stop/send/cancel"), so no acoustic wake word is
    // needed and the catalog lists them everywhere.
    out.push(CatalogEntry {
        category: "Control recording".to_string(),
        description: "Stop and paste".to_string(),
        phrases: vec!["zephyr stop".to_string()],
    });
    out.push(CatalogEntry {
        category: "Control recording".to_string(),
        description: "Stop, paste, then send in chat apps".to_string(),
        phrases: vec!["zephyr send".to_string()],
    });
    out.push(CatalogEntry {
        category: "Control recording".to_string(),
        description: "Cancel and discard".to_string(),
        phrases: vec!["zephyr cancel".to_string()],
    });

    // Standalone hand-actions (DIRECT_COMMANDS) — only the allowlisted
    // descriptions make the panel. The full table still powers matching; the
    // OS-shell admin verbs (Task Manager, Run dialog, Action Center, …) and
    // bare keypresses (Up/Down/Left/Right/Home/End) stay available by voice
    // but don't crowd the Standalone panel. macOS support gates the row at
    // emit time too.
    out.extend(grouped(
        "Standalone commands",
        DIRECT_COMMANDS
            .iter()
            .filter(|(_phrase, _keys, desc)| STANDALONE_DESCRIPTIONS.contains(desc))
            .filter(|(_phrase, keys, _desc)| command_token_supported_on_current_platform(keys))
            .map(|(phrase, _keys, desc)| (*phrase, *desc)),
    ));

    // Text case (TRANSFORM_PHRASES) — these are inline transforms, not
    // stand-alone actions, so they live on the FREE Voice Commands surface,
    // not here. Excluded intentionally.
    #[allow(unused)]
    fn transform_label(t: &str) -> &'static str {
        match t {
            "upper" => "Uppercase the last paste",
            "lower" => "Lowercase the last paste",
            "title" => "Title-case the last paste",
            "capitalize" => "Capitalize the last paste",
            _ => "Transform the last paste",
        }
    }

    out
}
