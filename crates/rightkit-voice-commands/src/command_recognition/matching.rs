use super::catalog::*;
use crate::command::CommandAction;

fn chord_number_word(k: &str) -> Option<&'static str> {
    Some(match k {
        "zero" => "0",
        "one" => "1",
        "two" => "2",
        "three" => "3",
        "four" => "4",
        "five" => "5",
        "six" => "6",
        "seven" => "7",
        "eight" => "8",
        "nine" => "9",
        _ => return None,
    })
}

fn is_named_key(k: &str) -> bool {
    const NAMED: &[&str] = &[
        "tab",
        "enter",
        "return",
        "space",
        "esc",
        "escape",
        "home",
        "end",
        "delete",
        "del",
        "insert",
        "backspace",
        "up",
        "down",
        "left",
        "right",
        "page up",
        "page down",
        "pageup",
        "pagedown",
    ];
    NAMED.contains(&k)
        || (k.starts_with('f')
            && k[1..]
                .parse::<u32>()
                .map(|n| (1..=24).contains(&n))
                .unwrap_or(false))
}

/// "control a" / "shift f5" / "alt one" -> canonical "ctrl+a" / "shift+f5".
pub(crate) fn parse_chord(norm: &str) -> Option<String> {
    let tokens: Vec<&str> = norm.split_whitespace().collect();
    if tokens.len() < 2 {
        return None;
    }
    let mut mods: Vec<&'static str> = Vec::new();
    let mut i = 0;
    while i < tokens.len() && CHORD_MODIFIERS.contains(&tokens[i]) {
        mods.push(normalize_mod(tokens[i])?);
        i += 1;
    }
    if mods.is_empty() {
        return None;
    }
    let key_tokens = &tokens[i..];
    if key_tokens.is_empty() {
        return None;
    }
    let key_joined = key_tokens.join(" ");
    let key: String = if let Some(n) = chord_number_word(&key_joined) {
        n.to_string()
    } else if is_named_key(&key_joined) {
        key_joined.replace(' ', "")
    } else if key_joined.chars().count() == 1
        && key_joined
            .chars()
            .next()
            .map(|c| c.is_ascii_alphanumeric())
            .unwrap_or(false)
    {
        key_joined
    } else {
        return None;
    };
    // Dedupe modifiers, preserving order, then append the key.
    let mut seen: Vec<&'static str> = Vec::new();
    for m in mods {
        if !seen.contains(&m) {
            seen.push(m);
        }
    }
    let mut result = seen.join("+");
    result.push('+');
    result.push_str(&key);
    Some(result)
}

// Only "open"/"launch" — NOT "start"/"run", which are common mid-dictation words
// ("run the tests", "start the meeting") and would mis-fire app launches.
const APP_LAUNCH_VERBS: &[&str] = &["open", "launch"];

pub fn app_alias(app: &str) -> Option<&'static str> {
    match app {
        "chrome" | "google chrome" => Some("chrome"),
        "firefox" => Some("firefox"),
        "edge" | "microsoft edge" | "msedge" => Some("msedge"),
        "explorer" | "file explorer" => Some("explorer"),
        "notepad" => Some("notepad"),
        "calculator" | "calc" => Some("calc"),
        "paint" | "ms paint" | "mspaint" => Some("mspaint"),
        "task manager" | "taskmgr" => Some("taskmgr"),
        "control panel" | "control" => Some("control"),
        "command prompt" | "cmd" => Some("cmd"),
        "powershell" => Some("powershell"),
        "terminal" | "windows terminal" | "wt" => Some("wt"),
        "settings" | "ms-settings:" => Some("ms-settings:"),
        "vs code" | "vscode" | "visual studio code" | "code" => Some("code"),
        "slack" => Some("slack"),
        "discord" => Some("discord"),
        "spotify" => Some("spotify"),
        "outlook" => Some("outlook"),
        "word" | "winword" => Some("winword"),
        "excel" => Some("excel"),
        "powerpoint" | "powerpnt" => Some("powerpnt"),
        "teams" => Some("teams"),
        "zoom" => Some("zoom"),
        "obs" => Some("obs"),
        "steam" => Some("steam"),
        _ => None,
    }
}

/// Pure grammar: if the transcript starts with an app-launch verb
/// (open/launch/start/run), return the raw spoken app name. Resolving that name
/// to an actual installed app is platform-specific and lives in the engine (it
/// must scan the system live), so this no longer consults the alias table — the
/// engine matches against what's actually installed.
pub fn app_launch_query(transcript: &str) -> Option<String> {
    let norm = normalize_for_match(transcript);
    let (verb, app) = norm.split_once(' ')?;
    if !APP_LAUNCH_VERBS.contains(&verb) {
        return None;
    }
    let app = app.trim();
    if app.is_empty() {
        return None;
    }
    Some(app.to_string())
}

/// Spoken triggers for the macOS Shortcuts bridge. The bare `"shortcut <name>"` /
/// `"shortcuts <name>"` forms are primary — ASR reliably mangles a leading "run"
/// ("run" -> "one"/"un"), so requiring it broke real use. "shortcut" is a rare
/// dictation word and a non-matching name still falls through to dictation, so
/// the collision risk is the same low bar as the app-launch "open"/"launch" verbs.
/// The "run shortcut …" variants stay accepted for anyone who says them.
const SHORTCUT_PREFIXES: &[&str] = &[
    "run shortcut ",
    "run the shortcut ",
    "run shortcuts ",
    "shortcut ",
    "shortcuts ",
];

/// Pure grammar: if the transcript starts with a shortcut-run trigger, return the
/// remaining spoken shortcut name (normalized lowercase, punctuation/filler
/// stripped). Resolving that name to an installed shortcut is platform-specific
/// and lives in the engine (`app_launch::resolve_shortcut`), which scans the
/// system live — so this never consults a table.
pub fn shortcut_query(transcript: &str) -> Option<String> {
    let norm = normalize_for_match(transcript);
    for prefix in SHORTCUT_PREFIXES {
        if let Some(rest) = norm.strip_prefix(prefix) {
            let name = rest.trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

fn sentinel_to_action(token: &str) -> Option<CommandAction> {
    let mouse = |action: &str,
                 button: Option<&str>,
                 clicks: Option<u32>,
                 direction: Option<&str>,
                 page: bool| {
        CommandAction::Mouse {
            action: action.to_string(),
            button: button.map(str::to_string),
            clicks,
            direction: direction.map(str::to_string),
            page,
        }
    };
    Some(match token {
        "__mouse_left" => mouse("click", Some("left"), Some(1), None, false),
        "__mouse_right" => mouse("click", Some("right"), Some(1), None, false),
        "__mouse_middle" => mouse("click", Some("middle"), Some(1), None, false),
        "__mouse_double" => mouse("click", Some("left"), Some(2), None, false),
        "__mouse_triple" => mouse("click", Some("left"), Some(3), None, false),
        "__scroll_up" => mouse("scroll", None, None, Some("up"), false),
        "__scroll_down" => mouse("scroll", None, None, Some("down"), false),
        "__scroll_left" => mouse("scroll", None, None, Some("left"), false),
        "__scroll_right" => mouse("scroll", None, None, Some("right"), false),
        "__scroll_page_up" => mouse("scroll", None, None, Some("up"), true),
        "__scroll_page_down" => mouse("scroll", None, None, Some("down"), true),
        "__always_on_top" => CommandAction::Special {
            op: "always_on_top".into(),
        },
        "__clear_clipboard" => CommandAction::Special {
            op: "clear_clipboard".into(),
        },
        "__backspace_last" => CommandAction::Special {
            op: "backspace_last".into(),
        },
        "__power_lock" => CommandAction::Power {
            op: "lock".into(),
            requires_confirm: true,
        },
        "__power_signout" => CommandAction::Power {
            op: "signout".into(),
            requires_confirm: true,
        },
        "__power_sleep" => CommandAction::Power {
            op: "sleep".into(),
            requires_confirm: true,
        },
        "__power_hibernate" => CommandAction::Power {
            op: "hibernate".into(),
            requires_confirm: true,
        },
        "__power_restart" => CommandAction::Power {
            op: "restart".into(),
            requires_confirm: true,
        },
        "__power_shutdown" => CommandAction::Power {
            op: "shutdown_pc".into(),
            requires_confirm: true,
        },
        "__power_cancel" => CommandAction::Power {
            op: "cancel_shutdown".into(),
            requires_confirm: false,
        },
        "__mac_force_quit" => CommandAction::Special {
            op: "mac_force_quit".into(),
        },
        _ => return None,
    })
}

pub(crate) fn token_to_action(token: &str, description: &str) -> CommandAction {
    if let Some(rest) = token.strip_prefix("__") {
        return sentinel_to_action(token).unwrap_or_else(|| CommandAction::Special {
            op: rest.to_string(),
        });
    }
    if token.contains(',') {
        let chords: Vec<String> = token
            .split(',')
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .collect();
        return CommandAction::KeySequence {
            chords,
            description: Some(description.to_string()),
        };
    }
    CommandAction::KeySequence {
        chords: vec![token.to_string()],
        description: Some(description.to_string()),
    }
}

/// Match an entire transcript against the standalone command catalog. Returns a
/// typed `CommandAction` if the transcript IS a command, else `None`. Order:
/// direct phrase → transform → chord grammar → app-launch grammar.
/// Levenshtein edit distance over chars (small inputs only).
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Nearest `DIRECT_COMMANDS` phrase within a conservative edit budget. Returns
/// `None` unless the input is command-length, the nearest is within budget, and
/// it is strictly closer than the runner-up (no ambiguous fires).
pub(crate) fn fuzzy_direct_match(norm: &str) -> Option<(&'static str, &'static str)> {
    let n = norm.chars().count();
    if !(3..=20).contains(&n) {
        return None;
    }
    let budget = if n <= 6 { 1 } else { 2 };
    let mut best: Option<(usize, &'static str, &'static str)> = None;
    let mut second = usize::MAX;
    for &(phrase, token, desc) in DIRECT_COMMANDS {
        // Fuzzy matching short one-word commands is unsafe in live dictation:
        // "end" matched "and", "right" matched "eight", etc. Keep the one
        // intentional single-word rescue ("undu" -> "undo"); require exact
        // speech for the rest.
        if !phrase.contains(' ') && phrase != "undo" {
            continue;
        }
        // Length gate trims obviously-different phrases cheaply.
        if phrase.chars().count().abs_diff(n) > budget {
            continue;
        }
        let d = edit_distance(norm, phrase);
        match best {
            Some((bd, _, _)) if d < bd => {
                second = bd;
                best = Some((d, token, desc));
            }
            Some((bd, _, _)) => {
                if d < second {
                    second = d;
                }
                let _ = bd;
            }
            None => best = Some((d, token, desc)),
        }
    }
    match best {
        Some((d, token, desc)) if d > 0 && d <= budget && d < second => Some((token, desc)),
        _ => None,
    }
}
