use super::*;
use crate::command::CommandAction;

#[test]
fn direct_phrase_select_all() {
    match recognize_command("select all").unwrap() {
        CommandAction::KeySequence { chords, .. } => assert_eq!(chords, vec!["ctrl+a"]),
        other => panic!("expected KeySequence, got {other:?}"),
    }
}

#[test]
fn comma_sequence_delete_line() {
    match recognize_command("delete line").unwrap() {
        CommandAction::KeySequence { chords, .. } => {
            assert_eq!(chords, vec!["home", "shift+end", "delete"]);
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn sentinel_mouse_double_click() {
    match recognize_command("double click").unwrap() {
        CommandAction::Mouse {
            action,
            button,
            clicks,
            ..
        } => {
            assert_eq!(action, "click");
            assert_eq!(button.as_deref(), Some("left"));
            assert_eq!(clicks, Some(2));
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn sentinel_power_commands_require_explicit_phrases() {
    assert!(recognize_command("lock").is_none());
    assert!(
        matches!(recognize_command("sleep computer"), Some(CommandAction::Power { op, requires_confirm: true }) if op == "sleep")
    );
    assert!(
        matches!(recognize_command("sign out"), Some(CommandAction::Power { op, requires_confirm: true }) if op == "signout")
    );
}

#[test]
fn transform_uppercase_with_whisper_variant() {
    assert!(
        matches!(recognize_command("uppercase that"), Some(CommandAction::LastPasteTransform { transform }) if transform == "upper")
    );
    assert!(
        matches!(recognize_command("apple case that"), Some(CommandAction::LastPasteTransform { transform }) if transform == "upper")
    );
}

#[test]
fn chord_grammar_spoken_modifier_plus_key() {
    assert!(
        matches!(recognize_command("control a"), Some(CommandAction::KeySequence { chords, .. }) if chords == ["ctrl+a"])
    );
    assert!(
        matches!(recognize_command("shift f5"), Some(CommandAction::KeySequence { chords, .. }) if chords == ["shift+f5"])
    );
    assert!(
        matches!(recognize_command("alt one"), Some(CommandAction::KeySequence { chords, .. }) if chords == ["alt+1"])
    );
    // win normalizes to windows
    assert!(
        matches!(recognize_command("windows d"), Some(CommandAction::KeySequence { chords, .. }) if chords == ["windows+d"])
    );
}

#[test]
fn app_launch_query_extracts_name_after_verb() {
    // The grammar returns the RAW spoken name; the engine resolves it against
    // installed apps. recognize_command itself no longer fires app launches.
    assert_eq!(app_launch_query("open chrome").as_deref(), Some("chrome"));
    assert_eq!(
        app_launch_query("launch visual studio code").as_deref(),
        Some("visual studio code")
    );
    // "start"/"run" are NOT launch verbs (too collision-prone with dictation).
    assert_eq!(app_launch_query("start the meeting"), None);
    assert_eq!(app_launch_query("run the tests"), None);
    // Not a launch verb / no app name → not an app-launch.
    assert_eq!(app_launch_query("just dictating here"), None);
    assert_eq!(app_launch_query("open"), None);
    assert!(recognize_command("open chrome").is_none());
}

#[test]
fn shortcut_query_extracts_name_after_run_shortcut() {
    // Returns the RAW spoken name; the engine resolves it against installed
    // shortcuts. Normalization lowercases + strips punctuation/filler.
    assert_eq!(
        shortcut_query("run shortcut Good Night").as_deref(),
        Some("good night")
    );
    assert_eq!(
        shortcut_query("Run the shortcut water eject.").as_deref(),
        Some("water eject")
    );
    assert_eq!(
        shortcut_query("run shortcuts goodnight please").as_deref(),
        Some("goodnight")
    );
    // Bare "shortcut <name>" / "shortcuts <name>" — the primary form, since ASR
    // mangles a leading "run".
    assert_eq!(shortcut_query("Shortcut test").as_deref(), Some("test"));
    assert_eq!(
        shortcut_query("shortcuts good night").as_deref(),
        Some("good night")
    );
    // Not the trigger / no name → not a shortcut request (falls through).
    assert_eq!(shortcut_query("run the tests"), None);
    assert_eq!(shortcut_query("open chrome"), None);
    assert_eq!(shortcut_query("run shortcut"), None);
    assert_eq!(shortcut_query("shortcut"), None);
    // recognize_command itself never fires a shortcut (resolved in the engine).
    assert!(recognize_command("run shortcut goodnight").is_none());
}

#[test]
fn normalization_strips_punctuation_and_filler() {
    // trailing punctuation + leading filler + trailing "please"
    assert!(recognize_command("Select all.").is_some());
    assert!(recognize_command("so copy").is_some());
    assert!(recognize_command("save please").is_some());
    assert!(recognize_command("undo!").is_some());
}

#[test]
fn non_commands_return_none() {
    assert!(recognize_command("the quick brown fox jumps over the lazy dog").is_none());
    assert!(recognize_command("hello there how are you").is_none());
    assert!(recognize_command("").is_none());
    assert!(recognize_command("   ").is_none());
    // a spoken modifier alone is not a chord
    assert!(recognize_command("control").is_none());
}

#[test]
fn and_is_not_mistaken_for_end_key() {
    // Regression (2026-07-01): bare "end" was a standalone command 1 edit
    // away from "and" — dictating a sentence starting with "And ..." fired
    // the End key and cut the recording. The phrase was removed; "end of
    // line" is unaffected since it's a 3-word phrase, not a fuzzy neighbor.
    assert!(recognize_command("And").is_none());
    assert!(recognize_command("and").is_none());
    assert!(recognize_command("end of line").is_some());
}

#[test]
fn raw_single_key_and_app_no_alias_passthrough() {
    for phrase in [
        "tab",
        "enter",
        "return",
        "space",
        "escape",
        "home",
        "print screen",
    ] {
        assert!(
            recognize_command(phrase).is_none(),
            "{phrase} should not be a standalone command"
        );
    }
    assert!(recognize_command("open someapp").is_none());
}

#[test]
fn standalone_directions_do_not_fire() {
    assert!(recognize_command("eight").is_none());
    assert!(recognize_command("light").is_none());
    assert!(recognize_command("click").is_none());
    for phrase in [
        "right",
        "left",
        "up",
        "down",
        "arrow right",
        "arrow left",
        "arrow up",
        "arrow down",
    ] {
        assert!(
            recognize_command(phrase).is_none(),
            "{phrase} should not be a command"
        );
    }
    match recognize_command("right click").unwrap() {
        CommandAction::Mouse {
            action,
            button,
            clicks,
            ..
        } => {
            assert_eq!(action, "click");
            assert_eq!(button.as_deref(), Some("right"));
            assert_eq!(clicks, Some(1));
        }
        other => panic!("expected right-click mouse action, got {other:?}"),
    }
    match recognize_command("left click").unwrap() {
        CommandAction::Mouse {
            action,
            button,
            clicks,
            ..
        } => {
            assert_eq!(action, "click");
            assert_eq!(button.as_deref(), Some("left"));
            assert_eq!(clicks, Some(1));
        }
        other => panic!("expected left-click mouse action, got {other:?}"),
    }
}

#[test]
fn exact_commands_can_also_be_longer_command_prefixes() {
    assert!(has_longer_direct_command_prefix("copy"));
    assert!(has_longer_direct_command_prefix("right"));
    assert!(recognize_command("right").is_none());
    assert!(!has_longer_direct_command_prefix("right click"));
    assert!(!has_longer_direct_command_prefix("screenshot"));
}

#[test]
fn parses_multi_modifier_chords_with_dedup() {
    assert!(matches!(
        recognize_command("control shift t"),
        Some(CommandAction::KeySequence { chords, .. }) if chords == ["ctrl+shift+t"]
    ));
    // duplicate modifier collapses
    assert!(matches!(
        recognize_command("control control a"),
        Some(CommandAction::KeySequence { chords, .. }) if chords == ["ctrl+a"]
    ));
}

#[test]
fn fuzzy_matches_mishears_but_not_arbitrary_speech() {
    // One-char mishear of a real command resolves.
    assert!(recognize_command("undu").is_some()); // -> undo
    assert!(recognize_command("save").is_some()); // -> save
                                                  // Exact still wins and is unaffected.
    assert!(recognize_command("undo").is_some());
    // Arbitrary / far speech does NOT fire a command.
    assert!(recognize_command("the weather is nice today").is_none());
    assert!(recognize_command("banana").is_none());
}

#[test]
fn inline_macros_are_not_standalone_commands() {
    for phrase in ["new line", "newline", "new paragraph"] {
        assert!(
            recognize_command(phrase).is_none(),
            "{phrase} belongs to inline dictation formatting"
        );
    }
}

#[test]
fn renamed_page_commands_work_without_bare_aliases() {
    assert!(recognize_command("refresh").is_none());
    assert!(recognize_command("reload").is_none());
    assert!(recognize_command("refresh page").is_some());
    assert!(recognize_command("reload page").is_some());
}

#[test]
fn catalog_groups_aliases_and_covers_categories() {
    let cat = command_catalog();
    assert!(!cat.is_empty());
    // Aliases collapse: "copy" + "copy that" under one "Copy" entry.
    let copy = cat
        .iter()
        .find(|e| e.description == "Copy")
        .expect("Copy entry present");
    assert!(copy.phrases.contains(&"copy".to_string()));
    assert!(copy.phrases.contains(&"copy that".to_string()));
    // Every catalog phrase is a real, recognized command (panel can't drift
    // from what actually fires) — checked for the control + editing groups.
    for e in &cat {
        if e.category == "Editing & shortcuts" {
            for p in &e.phrases {
                assert!(
                    recognize_command(p).is_some(),
                    "catalog phrase '{p}' does not recognize"
                );
            }
        }
    }
    // Categories present. "Control recording" lists the in-stream zephyr tail
    // triggers, which now work on both platforms (transcript-tail detection).
    let cats: std::collections::HashSet<_> = cat.iter().map(|e| e.category.as_str()).collect();
    assert!(cats.contains("Control recording"));
    assert!(cats.contains("Standalone commands"));
    assert!(cat
        .iter()
        .flat_map(|entry| entry.phrases.iter())
        .all(|phrase| !phrase.contains("submit")));
    // Inline transforms live on the FREE Voice Commands surface, not here,
    // so they MUST NOT appear in the standalone catalog (2026-07-06).
    assert!(!cats.contains("Text case"));
    assert!(!cats.contains("Editing & shortcuts"));
    // Every standalone description in the panel matches a row in the
    // recognizer's static table — the panel can never drift from what
    // actually fires.
    for entry in &cat {
        if entry.category == "Standalone commands" {
            for phrase in &entry.phrases {
                assert!(
                    recognize_command(phrase).is_some(),
                    "standalone catalog phrase '{phrase}' does not recognize",
                );
            }
        }
    }
}

#[test]
fn macos_command_remaps_and_hides_windows_only() {
    use MacCommand::*;
    // Switch window must become ⌘Tab on macOS (the reported bug).
    assert_eq!(macos_command("alt+tab"), Remap("cmd+tab"));
    // Next tab keeps REAL Control (not the ctrl→⌘ default).
    assert_eq!(macos_command("ctrl+tab"), Remap("ctrl+tab"));
    // Toggle keys / Windows-only have no macOS equivalent.
    assert_eq!(macos_command("caps lock"), Unsupported);
    assert_eq!(macos_command("windows+left"), Unsupported);
    assert_eq!(macos_command("ctrl+shift+esc"), Unsupported);
    assert_eq!(macos_command("windows+l"), Unsupported);
    // Plain editing chords ride the default ctrl→⌘ remap.
    assert_eq!(macos_command("ctrl+c"), Default);
    // A sequence is supported iff every sub-chord is.
    assert_eq!(macos_command("home, shift+end, delete"), Default);
}
