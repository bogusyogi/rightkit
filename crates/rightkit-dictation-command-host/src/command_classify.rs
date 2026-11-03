//! Single source of truth for "is this final transcript a standalone command?".
//!
//! Shared by the runtime finalize (to DISPATCH) and the worker's streaming
//! auto-fire (to decide an early hands-free STOP). Keeping ONE classifier means
//! the two can never drift on WHAT counts as a command — the bug where the worker
//! auto-fired catalog commands (screenshot) but not app-launch ("open chrome")
//! because it re-implemented only a subset of finalize.
//!
//! Control tails (`zephyr stop/send/cancel`) are NOT handled here — they transform
//! the transcript (strip the tail / cancel) rather than terminate it as a command,
//! so callers parse those separately via `text_pipeline::parse_control_command`.

use rightkit_voice_commands::command::CommandAction;

#[derive(Debug)]
pub enum CommandClassification {
    Complete(CommandAction),
    AmbiguousComplete(CommandAction),
    Prefix,
    None,
}

/// Resolve a final transcript to the standalone command it should dispatch, or
/// `None` for plain dictation. ALL standalone commands are Pro (catalog, app-launch,
/// and custom Shortcuts); free users only get inline formatting + zephyr
/// stop/send/cancel, which are handled elsewhere via `text_pipeline`. Final
/// app-launch and shortcut resolution stay authoritative; streaming uses only
/// bounded indexes so ordinary ASR probes never spawn a blocking subprocess.
pub fn classify_action(transcript: &str, is_pro: bool) -> Option<CommandAction> {
    match classify(transcript, is_pro, false) {
        CommandClassification::Complete(action)
        | CommandClassification::AmbiguousComplete(action) => Some(action),
        CommandClassification::Prefix | CommandClassification::None => None,
    }
}

pub fn classify_streaming(transcript: &str, is_pro: bool) -> CommandClassification {
    classify(transcript, is_pro, true)
}

fn classify(transcript: &str, is_pro: bool, streaming: bool) -> CommandClassification {
    classify_with_shortcut_resolvers_on_platform(
        transcript,
        is_pro,
        streaming,
        crate::app_launch::resolve_shortcut,
        crate::app_launch::resolve_shortcut_streaming,
        crate::app_launch::shortcut_streaming_is_ambiguous,
        cfg!(target_os = "macos"),
    )
}

fn classify_with_shortcut_resolvers_on_platform(
    transcript: &str,
    is_pro: bool,
    streaming: bool,
    resolve_shortcut: impl Fn(&str) -> Option<String>,
    resolve_shortcut_streaming: impl Fn(&str) -> Option<String>,
    streaming_shortcut_is_ambiguous: impl Fn(&str) -> bool,
    shortcut_platform_supported: bool,
) -> CommandClassification {
    // ALL standalone commands are Pro. Free users only get inline formatting +
    // zephyr stop/send/cancel (handled elsewhere via text_pipeline), so a non-Pro
    // transcript is never a standalone command here — return early as dictation.
    if !is_pro {
        return CommandClassification::None;
    }
    // 1. Catalog commands + chords + single keys + transforms (whole-utterance):
    //    undo/copy/screenshot/switch-window etc.
    if let Some(action) =
        rightkit_voice_commands::command_recognition::recognize_command(transcript)
    {
        if !action_supported_on_current_platform(&action) {
            return CommandClassification::None;
        }
        if rightkit_voice_commands::command_recognition::has_longer_direct_command_prefix(
            transcript,
        ) {
            return CommandClassification::AmbiguousComplete(action);
        }
        return CommandClassification::Complete(action);
    }
    // 2. Shortcut ("run shortcut good night"). The TRIGGER (shortcut_query) is
    //    platform-agnostic; only EXECUTION differs. Streaming resolves against
    //    the completed macOS catalog snapshot, while final classification keeps
    //    the live authoritative fallback.
    if let Some(query) = rightkit_voice_commands::command_recognition::shortcut_query(transcript) {
        let resolved = if streaming {
            resolve_shortcut_streaming(&query)
        } else {
            resolve_shortcut(&query)
        };
        if let Some(name) = resolved {
            if streaming && streaming_shortcut_is_ambiguous(&query) {
                return CommandClassification::AmbiguousComplete(CommandAction::RunShortcut {
                    name,
                });
            }
            return CommandClassification::Complete(CommandAction::RunShortcut { name });
        }
        if shortcut_platform_supported {
            return CommandClassification::Prefix;
        }
        return CommandClassification::None;
    }
    // 3. App launch ("open figma"). Streaming uses aliases + a prewarmed app
    //    index only, so partial ASR results never block capture coordination.
    if let Some(query) = rightkit_voice_commands::command_recognition::app_launch_query(transcript)
    {
        let resolved = if streaming {
            crate::app_launch::resolve_streaming(&query)
        } else {
            crate::app_launch::resolve(&query)
        };
        if let Some(name) = resolved {
            return CommandClassification::Complete(CommandAction::LaunchApp { name });
        }
        return CommandClassification::Prefix;
    }
    if command_prefix(transcript) {
        return CommandClassification::Prefix;
    }
    CommandClassification::None
}

fn action_supported_on_current_platform(action: &CommandAction) -> bool {
    match action {
        CommandAction::KeySequence { chords, .. } => chords.iter().all(|chord| {
            rightkit_voice_commands::command_recognition::command_token_supported_on_current_platform(chord)
        }),
        CommandAction::Special { op } if op == "mac_force_quit" => cfg!(target_os = "macos"),
        _ => true,
    }
}

fn command_prefix(transcript: &str) -> bool {
    let norm = normalize_prefix(transcript);
    if norm.is_empty() {
        return false;
    }
    rightkit_voice_commands::command_recognition::has_longer_direct_command_prefix(&norm)
        || matches!(norm.as_str(), "open" | "launch")
        || matches!(
            norm.as_str(),
            "control"
                | "ctrl"
                | "shift"
                | "alt"
                | "option"
                | "win"
                | "windows"
                | "super"
                | "meta"
                | "command"
                | "cmd"
        )
}

fn normalize_prefix(input: &str) -> String {
    input
        .trim()
        .trim_matches(|c: char| !c.is_alphanumeric() && !c.is_whitespace())
        .to_lowercase()
        .replace('-', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
