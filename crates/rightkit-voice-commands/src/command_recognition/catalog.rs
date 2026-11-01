// Recognition-only port of the Python `command_recognition.py` router.
//
// Decides IF an after-recording-stop transcript is a standalone command and
// WHAT typed `CommandAction` it requests. NO OS side effects — dispatch lives
// in the host application. Pure + unit-tested here (a GUI-free crate whose
// tests actually run), so the catalog is verified
// without a mic or the app. 1:1 with the Python module (catalog + 3 grammars:
// direct phrase → chord → app-launch). Cutover Phase F.

use std::collections::HashMap;
use std::sync::OnceLock;

/// (phrase) -> (action_token, description). action_token is a chord ("ctrl+a"),
/// a comma sequence ("home, shift+end, delete"), a single key ("tab"), or a
/// `__sentinel`. Copied 1:1 from the Python `_DIRECT_COMMANDS`.
pub(crate) const DIRECT_COMMANDS: &[(&str, &str, &str)] = &[
    // Text edit
    ("backspace", "backspace", "Backspace"),
    ("delete word", "ctrl+backspace", "Delete word back"),
    ("delete previous word", "ctrl+backspace", "Delete word back"),
    ("delete next word", "ctrl+delete", "Delete word forward"),
    ("delete word forward", "ctrl+delete", "Delete word forward"),
    ("delete line", "home, shift+end, delete", "Delete line"),
    (
        "delete sentence",
        "home, shift+end, delete",
        "Delete sentence",
    ),
    (
        "delete to end",
        "shift+end, delete",
        "Delete to end of line",
    ),
    (
        "delete to start",
        "shift+home, delete",
        "Delete to start of line",
    ),
    ("select word", "ctrl+shift+left", "Select word back"),
    ("select line", "home, shift+end", "Select line"),
    ("select sentence", "home, shift+end", "Select sentence"),
    ("undo", "ctrl+z", "Undo"),
    ("redo", "ctrl+shift+z", "Redo"),
    ("select all", "ctrl+a", "Select all"),
    ("copy", "ctrl+c", "Copy"),
    ("copy that", "ctrl+c", "Copy"),
    ("paste", "ctrl+v", "Paste"),
    ("paste that", "ctrl+v", "Paste"),
    ("cut", "ctrl+x", "Cut"),
    ("save", "ctrl+s", "Save"),
    ("save it", "ctrl+s", "Save"),
    ("save that", "ctrl+s", "Save"),
    ("delete that", "__backspace_last", "Backspace last paste"),
    ("scratch that", "__backspace_last", "Backspace last paste"),
    // Mouse
    ("left click", "__mouse_left", "Left click"),
    ("right click", "__mouse_right", "Right click"),
    ("middle click", "__mouse_middle", "Middle click"),
    ("double click", "__mouse_double", "Double click"),
    ("triple click", "__mouse_triple", "Triple click"),
    ("scroll up", "__scroll_up", "Scroll up"),
    ("scroll down", "__scroll_down", "Scroll down"),
    ("scroll left", "__scroll_left", "Scroll left"),
    ("scroll right", "__scroll_right", "Scroll right"),
    ("page scroll up", "__scroll_page_up", "Page scroll up"),
    ("page scroll down", "__scroll_page_down", "Page scroll down"),
    // Text formatting on selection
    ("bold that", "ctrl+b", "Bold"),
    ("italic that", "ctrl+i", "Italic"),
    ("italicize that", "ctrl+i", "Italic"),
    ("underline that", "ctrl+u", "Underline"),
    (
        "strikethrough that",
        "ctrl+shift+x",
        "Strikethrough (varies per app)",
    ),
    // Clipboard
    ("paste plain", "ctrl+shift+v", "Paste without formatting"),
    (
        "paste plain text",
        "ctrl+shift+v",
        "Paste without formatting",
    ),
    ("clipboard history", "windows+v", "Clipboard history"),
    ("clear clipboard", "__clear_clipboard", "Clear clipboard"),
    // Window: monitors
    (
        "move to next monitor",
        "windows+shift+right",
        "Move to next monitor",
    ),
    (
        "move to previous monitor",
        "windows+shift+left",
        "Move to previous monitor",
    ),
    (
        "move to right screen",
        "windows+shift+right",
        "Move to next monitor",
    ),
    (
        "move to left screen",
        "windows+shift+left",
        "Move to previous monitor",
    ),
    ("always on top", "__always_on_top", "Toggle always-on-top"),
    // Display / accessibility
    ("magnifier", "windows+=", "Open magnifier"),
    ("magnifier zoom in", "windows+=", "Magnifier zoom in"),
    ("magnifier zoom out", "windows+-", "Magnifier zoom out"),
    ("close magnifier", "windows+escape", "Close magnifier"),
    (
        "high contrast",
        "left alt+left shift+printscreen",
        "High contrast",
    ),
    // Power — typed sentinels
    ("sign out", "__power_signout", "Sign out"),
    ("log out", "__power_signout", "Sign out"),
    ("sleep computer", "__power_sleep", "Sleep"),
    ("hibernate computer", "__power_hibernate", "Hibernate"),
    ("restart computer", "__power_restart", "Restart (60s grace)"),
    (
        "shutdown computer",
        "__power_shutdown",
        "Shutdown (60s grace)",
    ),
    ("cancel shutdown", "__power_cancel", "Cancel shutdown"),
    // Single keys
    ("delete", "delete", "Delete"),
    ("del", "delete", "Delete"),
    ("forward delete", "delete", "Forward delete"),
    // Find / zoom
    ("find", "ctrl+f", "Find"),
    ("find next", "f3", "Find next"),
    ("zoom in", "ctrl+=", "Zoom in"),
    ("zoom out", "ctrl+-", "Zoom out"),
    ("reset zoom", "ctrl+0", "Reset zoom"),
    // Window / tab navigation
    ("alt tab", "alt+tab", "Switch window"),
    ("alt tag", "alt+tab", "Switch window"),
    ("alt tap", "alt+tab", "Switch window"),
    ("switch window", "alt+tab", "Switch window"),
    ("switch windows", "alt+tab", "Switch window"),
    ("switch app", "alt+tab", "Switch window"),
    ("switch apps", "alt+tab", "Switch window"),
    ("next tab", "ctrl+tab", "Next tab"),
    ("switch tab", "ctrl+tab", "Next tab"),
    ("switch tabs", "ctrl+tab", "Next tab"),
    ("previous tab", "ctrl+shift+tab", "Previous tab"),
    ("last tab", "ctrl+shift+tab", "Previous tab"),
    ("new tab", "ctrl+t", "New tab"),
    ("close tab", "ctrl+w", "Close tab"),
    ("reopen tab", "ctrl+shift+t", "Reopen tab"),
    ("refresh page", "f5", "Refresh page"),
    ("reload page", "ctrl+r", "Reload page"),
    ("go back", "alt+left", "Back"),
    ("go forward", "alt+right", "Forward"),
    ("address bar", "ctrl+l", "Address bar"),
    ("close window", "alt+f4", "Close window"),
    ("minimize", "windows+down", "Minimize"),
    ("maximize", "windows+up", "Maximize"),
    ("snap left", "windows+left", "Snap left"),
    ("snap right", "windows+right", "Snap right"),
    ("show desktop", "windows+d", "Show desktop"),
    ("task view", "windows+tab", "Task view"),
    ("lock screen", "windows+l", "Lock"),
    // Dispatcher intercepts this semantic chord and runs native monitor
    // capture, then applies the user's screenshot destination setting.
    (
        "screenshot",
        "windows+printscreen",
        "Screenshot (full screen)",
    ),
    (
        "take screenshot",
        "windows+printscreen",
        "Screenshot (full screen)",
    ),
    // Semantic token, NOT a chord: the engine finalize intercepts it and
    // routes to the summarize-selection lane (captured UIA/AX selection,
    // else a clipboard-preserving Ctrl+C fetch) -> L3 summary -> paste or
    // clip. Listed here so the worker's standalone-command probe AUTO-FIRES
    // on the bare word like any other command.
    ("summarize", "__summarize_selection", "Summarize selection"),
    ("summarise", "__summarize_selection", "Summarize selection"),
    ("open explorer", "windows+e", "File Explorer"),
    ("file explorer", "windows+e", "File Explorer"),
    ("task manager", "ctrl+shift+esc", "Task Manager"),
    ("force quit", "__mac_force_quit", "Force Quit"),
    ("run dialog", "windows+r", "Run"),
    ("open settings", "windows+i", "Settings"),
    ("action center", "windows+a", "Action Center"),
    ("notifications", "windows+n", "Notifications"),
    ("top of page", "ctrl+home", "Top"),
    ("bottom of page", "ctrl+end", "Bottom"),
    ("start of line", "home", "Start of line"),
    ("end of line", "end", "End of line"),
    ("page up", "page up", "Page up"),
    ("page down", "page down", "Page down"),
    ("indent", "tab", "Indent"),
    ("outdent", "shift+tab", "Outdent"),
    ("indent that", "tab", "Indent"),
    ("outdent that", "shift+tab", "Outdent"),
    ("previous window", "alt+shift+tab", "Previous window"),
    ("new desktop", "ctrl+windows+d", "New desktop"),
    ("next desktop", "ctrl+windows+right", "Next desktop"),
    ("previous desktop", "ctrl+windows+left", "Previous desktop"),
    ("volume up", "volume up", "Volume up"),
    ("volume down", "volume down", "Volume down"),
    ("mute", "volume mute", "Mute"),
    ("play pause", "play/pause media", "Play/Pause"),
    ("next track", "next track", "Next track"),
    ("previous track", "previous track", "Previous track"),
    ("windows search", "windows+s", "Windows search"),
];

/// Last-paste casing transforms (phrase -> transform key). 1:1 with Python.
pub(crate) const TRANSFORM_PHRASES: &[(&str, &str)] = &[
    ("uppercase that", "upper"),
    ("upper case that", "upper"),
    ("apple case that", "upper"), // Whisper mishearing
    ("all caps that", "upper"),
    ("caps that", "upper"),
    ("make it uppercase", "upper"),
    ("make that uppercase", "upper"),
    ("lowercase that", "lower"),
    ("lower case that", "lower"),
    ("make it lowercase", "lower"),
    ("title case that", "title"),
    ("capitalise that", "capitalize"),
    ("capitalize that", "capitalize"),
];

pub(crate) fn direct_map() -> &'static HashMap<&'static str, (&'static str, &'static str)> {
    static MAP: OnceLock<HashMap<&'static str, (&'static str, &'static str)>> = OnceLock::new();
    MAP.get_or_init(|| {
        DIRECT_COMMANDS
            .iter()
            .map(|(p, t, d)| (*p, (*t, *d)))
            .collect()
    })
}

pub(crate) fn transform_map() -> &'static HashMap<&'static str, &'static str> {
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| TRANSFORM_PHRASES.iter().map(|(p, t)| (*p, *t)).collect())
}

const FILLER_PREFIXES: &[&str] = &["and ", "so ", "okay ", "ok ", "please ", "um ", "uh "];
const FILLER_SUFFIXES: &[&str] = &[" please", " thanks", " thank you"];

/// Lowercase, strip terminal/leading punctuation, hyphen→space, collapse
/// whitespace, drop leading/trailing Whisper filler words. 1:1 with Python.
pub(crate) fn normalize_for_match(text: &str) -> String {
    let mut s = text.to_lowercase();
    s = s.trim().to_string();
    let punct: &[char] = &['.', '!', '?', ',', ';', ':', '"', '\''];
    s = s.trim_end_matches(punct).trim().to_string();
    s = s.trim_start_matches(punct).trim().to_string();
    s = s.replace('-', " ");
    // Collapse runs of whitespace to a single space.
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    // Strip leading filler words (repeatedly).
    loop {
        let mut changed = false;
        for p in FILLER_PREFIXES {
            if let Some(rest) = s.strip_prefix(p) {
                s = rest.to_string();
                changed = true;
                break;
            }
        }
        if !changed {
            break;
        }
    }
    // Strip a trailing filler word (single pass, matching Python).
    for suf in FILLER_SUFFIXES {
        if let Some(head) = s.strip_suffix(suf) {
            s = head.trim().to_string();
            break;
        }
    }
    s
}

pub fn has_longer_direct_command_prefix(text: &str) -> bool {
    let norm = normalize_for_match(text);
    if norm.is_empty() {
        return false;
    }
    let prefix = format!("{norm} ");
    DIRECT_COMMANDS
        .iter()
        .any(|(phrase, _token, _desc)| phrase.starts_with(&prefix))
}

pub(crate) const CHORD_MODIFIERS: &[&str] = &[
    "control", "ctrl", "shift", "alt", "win", "windows", "super", "meta", "command", "cmd",
];

pub(crate) fn normalize_mod(m: &str) -> Option<&'static str> {
    Some(match m {
        "control" | "ctrl" => "ctrl",
        "shift" => "shift",
        "alt" | "option" => "alt",
        "win" | "windows" | "super" | "meta" | "command" | "cmd" => "windows",
        _ => return None,
    })
}
