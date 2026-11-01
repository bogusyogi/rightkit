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

#[cfg(test)]
fn classify_with_shortcut_resolver(
    transcript: &str,
    is_pro: bool,
    streaming: bool,
    resolve_shortcut: impl Fn(&str) -> Option<String>,
) -> CommandClassification {
    classify_with_shortcut_resolvers_on_platform(
        transcript,
        is_pro,
        streaming,
        resolve_shortcut,
        |_| None,
        |_| false,
        true,
    )
}

#[cfg(test)]
fn classify_with_shortcut_resolvers(
    transcript: &str,
    is_pro: bool,
    streaming: bool,
    resolve_shortcut: impl Fn(&str) -> Option<String>,
    resolve_shortcut_streaming: impl Fn(&str) -> Option<String>,
) -> CommandClassification {
    classify_with_shortcut_resolvers_on_platform(
        transcript,
        is_pro,
        streaming,
        resolve_shortcut,
        resolve_shortcut_streaming,
        |_| false,
        true,
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

#[cfg(test)]
mod gate_tests {
    use super::*;
    // All standalone commands are Pro (locked 2026-06-30): on a free license every
    // catalog command stays plain dictation; with Pro it dispatches. Free users keep
    // inline formatting + zephyr stop/send/cancel, which are handled in text_pipeline.
    #[test]
    fn catalog_commands_require_pro() {
        assert!(classify_action("switch window", false).is_none());
        assert!(classify_action("Screenshot.", false).is_none());
        assert!(classify_action("undo", false).is_none());
        assert!(classify_action("switch window", true).is_some());
        assert!(classify_action("undo", true).is_some());
    }
    #[test]
    fn plain_dictation_is_not_a_command() {
        assert!(classify_action("checking to see if this works", false).is_none());
    }

    #[test]
    fn incomplete_launch_waits_for_next_word() {
        assert!(matches!(
            classify_streaming("open", true),
            CommandClassification::Prefix
        ));
        assert!(matches!(
            classify_streaming("launch", true),
            CommandClassification::Prefix
        ));
    }

    #[test]
    fn shortcut_lookup_is_deferred_until_final_classification() {
        for transcript in ["shortcut good night", "run shortcut good night"] {
            let streaming = classify_with_shortcut_resolvers(
                transcript,
                true,
                true,
                |_| panic!("streaming classification must not use final resolver"),
                |_| None,
            );
            assert!(matches!(streaming, CommandClassification::Prefix));

            let calls = std::cell::Cell::new(0);
            let final_classification =
                classify_with_shortcut_resolver(transcript, true, false, |query| {
                    calls.set(calls.get() + 1);
                    assert_eq!(query, "good night");
                    Some("Good Night".to_string())
                });
            assert!(matches!(
                final_classification,
                CommandClassification::Complete(CommandAction::RunShortcut { name })
                    if name == "Good Night"
            ));
            assert_eq!(calls.get(), 1);
        }
    }

    #[test]
    fn streaming_shortcut_candidate_requires_catalog_match() {
        let complete = classify_with_shortcut_resolvers(
            "shortcut good night",
            true,
            true,
            |_| panic!("streaming classification must not use final resolver"),
            |query| {
                assert_eq!(query, "good night");
                Some("Good Night".to_string())
            },
        );
        assert!(matches!(
            complete,
            CommandClassification::Complete(CommandAction::RunShortcut { name })
                if name == "Good Night"
        ));

        let incomplete =
            classify_with_shortcut_resolvers("shortcut good", true, true, |_| None, |_| None);
        assert!(matches!(incomplete, CommandClassification::Prefix));

        let unknown =
            classify_with_shortcut_resolvers("shortcut unknown", true, true, |_| None, |_| None);
        assert!(matches!(unknown, CommandClassification::Prefix));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn shortcut_grammar_is_not_a_command_on_unknown_platform() {
        assert!(matches!(
            classify_streaming("shortcut good night", true),
            CommandClassification::None
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn production_streaming_shortcut_classification_uses_cached_catalog() {
        let _guard = crate::test_support::test_env_guard();

        crate::app_launch::set_shortcut_cache_for_tests(None);
        assert!(matches!(
            classify_streaming("shortcut good night", true),
            CommandClassification::Prefix
        ));

        crate::app_launch::set_shortcut_cache_for_tests(Some(vec!["Good Night".to_string()]));
        assert!(matches!(
            classify_streaming("shortcut good night", true),
            CommandClassification::Complete(CommandAction::RunShortcut { name })
                if name == "Good Night"
        ));

        crate::app_launch::set_shortcut_cache_for_tests(Some(vec![
            "Good Night".to_string(),
            "Good-Night".to_string(),
        ]));
        assert!(matches!(
            classify_streaming("shortcut good night", true),
            CommandClassification::Prefix
        ));

        crate::app_launch::set_shortcut_cache_for_tests(Some(vec![
            "Good".to_string(),
            "Good Night".to_string(),
        ]));
        assert!(matches!(
            classify_streaming("shortcut good", true),
            CommandClassification::AmbiguousComplete(CommandAction::RunShortcut { name })
                if name == "Good"
        ));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn common_windows_apps_complete_from_aliases_without_live_scan() {
        for transcript in [
            "open chrome",
            "open edge",
            "open calculator",
            "open command prompt",
        ] {
            assert!(matches!(
                classify_streaming(transcript, true),
                CommandClassification::Complete(CommandAction::LaunchApp { .. })
            ));
        }
    }

    #[test]
    fn modifier_prefix_waits_for_key() {
        assert!(matches!(
            classify_streaming("control", true),
            CommandClassification::Prefix
        ));
    }

    #[test]
    fn direct_catalog_prefix_waits_but_bare_shortcut_does_not() {
        assert!(matches!(
            classify_streaming("right", true),
            CommandClassification::Prefix
        ));
        assert!(matches!(
            classify_streaming("delete", true),
            CommandClassification::Prefix | CommandClassification::AmbiguousComplete(_)
        ));
        assert!(matches!(
            classify_streaming("shortcut", true),
            CommandClassification::None
        ));
    }

    #[test]
    fn exact_prefix_command_waits_in_streaming_but_dispatches_final() {
        assert!(matches!(
            classify_streaming("copy", true),
            CommandClassification::AmbiguousComplete(_)
        ));
        assert!(matches!(
            classify_action("copy", true),
            Some(CommandAction::KeySequence { .. })
        ));
        assert!(matches!(
            classify_streaming("right click", true),
            CommandClassification::Complete(CommandAction::Mouse { .. })
        ));
        assert!(matches!(
            classify_streaming("right", true),
            CommandClassification::Prefix
        ));
        assert!(matches!(
            classify_streaming("eight", true),
            CommandClassification::None
        ));
    }

    #[test]
    fn platform_specific_commands_do_not_cross_os_lines() {
        #[cfg(target_os = "windows")]
        {
            assert!(matches!(
                classify_streaming("task manager", true),
                CommandClassification::Complete(CommandAction::KeySequence { .. })
            ));
            assert!(matches!(
                classify_streaming("force quit", true),
                CommandClassification::None
            ));
        }
        #[cfg(target_os = "macos")]
        {
            assert!(matches!(
                classify_streaming("task manager", true),
                CommandClassification::None
            ));
            assert!(matches!(
                classify_streaming("force quit", true),
                CommandClassification::Complete(CommandAction::Special { .. })
            ));
        }
    }

    #[test]
    fn unresolved_launch_phrase_stays_prefix_until_worker_timeout() {
        assert!(matches!(
            classify_streaming("open the document after lunch", true),
            CommandClassification::Prefix
        ));
    }

    #[test]
    fn incomplete_app_decodes_do_not_launch_a_fuzzy_app() {
        for transcript in [
            "open ed",
            "open cat",
            "open cal",
            "open calculation",
            "open calculate",
            "open cross",
        ] {
            assert!(matches!(
                classify_streaming(transcript, true),
                CommandClassification::Prefix
            ));
        }
    }

    #[test]
    fn unrelated_dictation_is_none() {
        assert!(matches!(
            classify_streaming("checking to see if this works", true),
            CommandClassification::None
        ));
    }

    #[test]
    fn standalone_commands_require_pro() {
        // All standalone commands are Pro: a free user's transcript never classifies,
        // even when it matches a catalog command or a launch prefix.
        assert!(classify_action("copy", false).is_none());
        assert!(matches!(
            classify_streaming("open", false),
            CommandClassification::None
        ));
        assert!(matches!(
            classify_streaming("open figma", false),
            CommandClassification::None
        ));
    }
}
