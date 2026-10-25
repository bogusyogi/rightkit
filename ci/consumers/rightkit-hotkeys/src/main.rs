// rightkit-hotkeys 0.1.2 consumed by exact registry pin (no features: the crate has none, and the
// apps use it that way), outside the workspace.
//
// Proves, through the public API only, the pure modules that compile and behave the same on
// Windows, macOS and Linux:
// - chord parsing: parse_chord reads modifiers and one key case-insensitively, normalises modifier
//   order and the cmd/option/esc/return aliases, and refuses an empty chord, a chord without a key,
//   two keys, an unknown token and a repeated modifier (ChordError); canonical spellings round-trip;
//   Chord::format for Mac (glyphs), Windows (Win) and Other; Key::parse and the macOS keycodes and
//   Windows virtual keys of a set of keys, F21 to F24 having no macOS code (NO_MAC_KEYCODE);
// - parse_modifier_token sides, Platform::current for the host, and the push-to-talk alias table
//   (PttAlias supported, parse, canonical, display, modifier for Windows, Mac and Other, and
//   parse_ptt_key_name on the host platform);
// - ChordRegistry refuses a chord bound to another owner (RegistryConflict) whatever the spelling,
//   is idempotent for the same owner and frees a chord on release; HotkeyConflict names the failed
//   required shortcut;
// - HoldTracker pairs every press with exactly one release through the KeyStateSource seam;
//   RegisterFailure maps Win32 codes and Carbon OSStatus values to typed failures;
// - the push-to-talk policy: PttHandoff start, end and completion in every order (a release
//   during start is applied when the start completes), the abort cause carried through a cancel,
//   format_abort, should_arm_ptt, the chord abort window and the decision predicates;
// - HookChords matches a chord over raw key events (exact modifiers, swallow of the chord key only,
//   one Pressed per hold, Released on key-up or modifier-up, injected events ignored, AlreadyBound,
//   unbind and release_all); Registrations drives a fake OS hotkey table (ids, MOD_NOREPEAT flags,
//   conflict and not-registered paths, stale records); hook_detail decodes key and mouse events;
//   MiddleClickGate, handle_other_mouse and run_job keep the lookup off the tap callback;
//   ArmTimer fires, cancels, re-arms and stops on drop.
// - on Windows and macOS only: the platform types are exported (checked by name at compile time,
//   nothing is constructed). Nothing runs on Linux beyond the pure modules.
// Does not prove: any native registration or tap. Carbon hotkeys, CGEventTap, FnHotkeyTap,
// MiddleMouseTap, RegisterHotKey, WH_KEYBOARD_LL and the message-loop threads need an interactive
// desktop session, Accessibility and Input Monitoring grants, or a real keyboard, so they are
// host-bound and left to the crate's own platform tests. Public API only.
// State: in-memory values and short-lived timer threads only. No network, no files, no process
// spawn, no user data, no real input.
use rightkit_hotkeys::chord::NO_MAC_KEYCODE;
use rightkit_hotkeys::decision::{other_mouse_down_is_middle, MOUSE_BUTTON_MIDDLE};
use rightkit_hotkeys::hook_detail::{
    classify_mouse_message, is_thread_message, KeyEventDetail, MouseEvent, MouseKind,
    LLKHF_ALTDOWN, LLKHF_EXTENDED, LLKHF_INJECTED, LLMHF_INJECTED, THREAD_MESSAGE_MAX,
    THREAD_MESSAGE_MIN,
};
use rightkit_hotkeys::middle_click::{
    handle_other_mouse, run_job, DownAction, Job, JobActions, MiddleClickGate, MouseEdge,
    Resolution, UpAction, Verdict, DECIDE_STALE, DIAG_OTHER_BUTTON,
};
use rightkit_hotkeys::ptt::{
    format_abort, run_feedback_before_action, should_abort_ptt_chord, should_arm_ptt,
    should_clear_ptt_hold_suppression, PttAbortCause, PttAbortKind, PttEnd, PttHandoff,
    PttModifiers, PttPhase, PTT_CHORD_ABORT_WINDOW_MS,
};
use rightkit_hotkeys::ptt_timer::{ArmTimer, MAX_ARM_DELAY};
use rightkit_hotkeys::registration::{
    hold_keys, modifier_bits, modifier_flags, ERROR_HOTKEY_NOT_REGISTERED,
};
use rightkit_hotkeys::{
    parse_chord, parse_modifier_token, parse_ptt_key_name, AlreadyBound, Chord, ChordError,
    ChordRegistry, HoldKeys, HoldTracker, HookChords, HookOutcome, HotkeyApi, HotkeyConflict,
    HotkeyState, Key, KeyStateSource, Modifier, Platform, PttAlias, RegisterFailure, Registrations,
    RegistryConflict, Side, UnregisterFailure,
};
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

fn chord(text: &str) -> Chord {
    parse_chord(text).unwrap_or_else(|error| panic!("`{text}` must parse: {error}"))
}

fn main() {
    chord_parsing_and_keys();
    modifiers_platform_and_ptt_aliases();
    registry_and_conflicts();
    hold_tracking_and_failures();
    push_to_talk_policy();
    hook_matcher();
    registration_table();
    hook_detail_decoding();
    middle_click_machinery();
    arm_timer();
    platform_surface();
    println!("OK rightkit-hotkeys 0.1.2 (registry consumer)");
}

// ---- chord parsing and keys ----------------------------------------------------------------

fn chord_parsing_and_keys() {
    let parsed = chord("ctrl + SHIFT + space");
    assert_eq!(parsed.modifiers, vec![Modifier::Ctrl, Modifier::Shift]);
    assert_eq!(parsed.key.name(), "space");
    assert_eq!(parsed.canonical(), "Ctrl+Shift+Space");
    assert!(
        parsed.has(Modifier::Ctrl) && parsed.has(Modifier::Shift) && !parsed.has(Modifier::Alt)
    );
    assert_eq!(
        chord("Shift+Ctrl+K"),
        chord("Ctrl+Shift+K"),
        "modifier order is normalised"
    );
    assert_eq!(chord("Cmd+Esc"), chord("Meta+Escape"));
    assert_eq!(chord("Option+Return"), chord("Alt+Enter"));
    assert_eq!(chord("Ctrl+.").key.name(), "period");
    assert_eq!(chord("Ctrl+`").key.name(), "grave");
    assert_eq!(chord("Ctrl+Del").key.name(), "delete");
    assert_eq!(
        chord("F5").modifiers,
        Vec::<Modifier>::new(),
        "a bare key is a chord"
    );

    assert_eq!(parse_chord(""), Err(ChordError::Empty));
    assert_eq!(parse_chord("   "), Err(ChordError::Empty));
    assert!(matches!(
        parse_chord("Ctrl+Shift"),
        Err(ChordError::NoKey(_))
    ));
    assert!(matches!(
        parse_chord("Ctrl+A+B"),
        Err(ChordError::MultipleKeys(_))
    ));
    assert!(
        matches!(parse_chord("Ctrl+Banana"), Err(ChordError::UnknownToken(t)) if t == "Banana")
    );
    assert!(matches!(
        parse_chord("Ctrl+Control+A"),
        Err(ChordError::DuplicateModifier(_))
    ));
    assert!(matches!(parse_chord("Ctrl++A"), Err(ChordError::UnknownToken(t)) if t.is_empty()));
    assert!(parse_chord("F25").is_err());
    for error in [
        ChordError::Empty,
        ChordError::NoKey("Ctrl".into()),
        ChordError::MultipleKeys("A+B".into()),
        ChordError::UnknownToken("x".into()),
        ChordError::DuplicateModifier("Ctrl".into()),
    ] {
        assert!(!error.to_string().is_empty());
    }

    for text in [
        "Ctrl+Alt+Delete",
        "Meta+Shift+F12",
        "Alt+,",
        "Win+Left",
        "Super+Alt+Z",
        "Ctrl+Alt+Shift+Meta+Up",
    ] {
        let chord = chord(text);
        assert_eq!(parse_chord(&chord.canonical()), Ok(chord.clone()), "{text}");
    }

    let all = chord("Ctrl+Shift+Meta+K");
    assert_eq!(all.format(Platform::Mac), "\u{2303}\u{21E7}\u{2318}K");
    assert_eq!(all.format(Platform::Windows), "Ctrl+Shift+Win+K");
    assert_eq!(all.format(Platform::Other), "Ctrl+Shift+Meta+K");
    assert_eq!(chord("Alt+,").format(Platform::Windows), "Alt+,");
    assert_eq!(
        chord("Alt+Super+Left").format(Platform::Windows),
        "Alt+Win+Left"
    );

    let space = Key::parse("space").expect("space");
    assert_eq!((space.mac_keycode(), space.windows_vk()), (49, 0x20));
    assert_eq!(space.mac_keycode_checked(), Some(49));
    for (name, mac, vk) in [
        ("a", 0, 0x41),
        ("enter", 36, 0x0D),
        ("escape", 53, 0x1B),
        ("f1", 122, 0x70),
        ("f20", 90, 0x83),
        ("left", 123, 0x25),
    ] {
        let key = Key::parse(name).unwrap_or_else(|| panic!("{name}"));
        assert_eq!((key.mac_keycode(), key.windows_vk()), (mac, vk), "{name}");
    }
    assert!(Key::parse("nope").is_none());
    assert!(Key::parse("ctrl").is_none(), "a modifier is not a key");
    for (name, vk) in [("f21", 0x84), ("F22", 0x85), ("f23", 0x86), ("F24", 0x87)] {
        let key = chord(&format!("Ctrl+{name}")).key;
        assert_eq!(key.windows_vk(), vk, "{name}");
        assert_eq!(
            key.mac_keycode_checked(),
            None,
            "{name} has no macOS virtual keycode"
        );
        assert_eq!(key.mac_keycode(), NO_MAC_KEYCODE);
    }
    assert_eq!(NO_MAC_KEYCODE, 0xFFFF);
}

// ---- modifiers, platform, push-to-talk aliases ---------------------------------------------

fn modifiers_platform_and_ptt_aliases() {
    let right_ctrl = parse_modifier_token("RCtrl").expect("rctrl");
    assert_eq!(
        (right_ctrl.modifier, right_ctrl.side),
        (Modifier::Ctrl, Side::Right)
    );
    assert_eq!(
        parse_modifier_token("command").map(|m| (m.modifier, m.side)),
        Some((Modifier::Meta, Side::Any))
    );
    assert_eq!(
        parse_modifier_token("lshift").map(|m| (m.modifier, m.side)),
        Some((Modifier::Shift, Side::Left))
    );
    assert_eq!(
        parse_modifier_token("rwin").map(|m| (m.modifier, m.side)),
        Some((Modifier::Win, Side::Right))
    );
    assert_eq!(
        parse_modifier_token("Option").map(|m| m.modifier),
        Some(Modifier::Alt)
    );
    assert!(parse_modifier_token("space").is_none());
    assert_eq!(Modifier::Ctrl.as_str(), "Ctrl");
    assert_eq!(Modifier::Meta.as_str(), "Meta");

    let expected = if cfg!(target_os = "macos") {
        Platform::Mac
    } else if cfg!(windows) {
        Platform::Windows
    } else {
        Platform::Other
    };
    assert_eq!(Platform::current(), expected);

    let names = |platform| {
        PttAlias::supported(platform)
            .iter()
            .map(|a| a.canonical())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(Platform::Windows), ["lalt", "ralt", "rctrl"]);
    assert_eq!(names(Platform::Mac), ["ctrl", "lalt", "ralt"]);
    assert_eq!(names(Platform::Other), ["ctrl", "lalt", "ralt", "rctrl"]);
    for (platform, bad) in [
        (
            Platform::Windows,
            ["ctrl", "lctrl", "shift", "Ctrl+Space", "f5"],
        ),
        (
            Platform::Mac,
            ["shift", "Ctrl+Space", "f5", "lshift", "win"],
        ),
    ] {
        for text in bad {
            let error = PttAlias::parse(text, platform).unwrap_err();
            assert!(
                error.contains("not a supported push-to-talk alias"),
                "{platform:?} {text}: {error}"
            );
        }
    }
    for text in ["ctrl", "Control", "lctrl", "RCtrl"] {
        assert_eq!(
            PttAlias::parse(text, Platform::Mac),
            Ok(PttAlias::Ctrl),
            "{text}"
        );
    }
    assert_eq!(
        PttAlias::parse("rctrl", Platform::Windows),
        Ok(PttAlias::RightCtrl)
    );
    assert_eq!(
        PttAlias::parse("Left Alt", Platform::Windows),
        Ok(PttAlias::LeftAlt),
        "spaces and case are ignored"
    );
    assert_eq!(
        PttAlias::parse("right_option", Platform::Mac),
        Ok(PttAlias::RightAlt)
    );
    for platform in [Platform::Windows, Platform::Mac, Platform::Other] {
        for alias in PttAlias::supported(platform) {
            assert_eq!(PttAlias::parse(alias.canonical(), platform), Ok(*alias));
            assert_eq!(
                PttAlias::parse(alias.display(platform), platform),
                Ok(*alias),
                "display parses back"
            );
            assert_eq!(
                PttAlias::parse(&alias.canonical().to_ascii_uppercase(), platform),
                Ok(*alias)
            );
        }
    }
    assert_eq!(PttAlias::LeftAlt.display(Platform::Mac), "Left Option");
    assert_eq!(PttAlias::LeftAlt.display(Platform::Windows), "Left Alt");
    assert_eq!(
        PttAlias::RightCtrl.display(Platform::Windows),
        "Right Control"
    );
    let ralt = PttAlias::RightAlt.modifier();
    assert_eq!((ralt.modifier, ralt.side), (Modifier::Alt, Side::Right));
    for alias in PttAlias::supported(Platform::current()) {
        assert_eq!(parse_ptt_key_name(alias.canonical()), Ok(alias.modifier()));
    }
    for bad in ["shift", "Ctrl+Space", "f5"] {
        assert!(parse_ptt_key_name(bad).is_err(), "{bad}");
    }
}

// ---- registry and conflicts ----------------------------------------------------------------

fn registry_and_conflicts() {
    let mut registry = ChordRegistry::new();
    assert!(registry.bind(&chord("Ctrl+Shift+K"), "palette").is_ok());
    assert!(registry.bind(&chord("Ctrl+Shift+J"), "search").is_ok());
    assert_eq!(registry.owner_of(&chord("Ctrl+Shift+K")), Some("palette"));
    registry.bind(&chord("Ctrl+Space"), "dictation").unwrap();
    let error = registry.bind(&chord("ctrl+space"), "launcher").unwrap_err();
    assert_eq!(
        error,
        RegistryConflict {
            chord: "Ctrl+Space".into(),
            held_by: "dictation".into(),
            requested_by: "launcher".into()
        }
    );
    assert_eq!(
        registry.owner_of(&chord("Ctrl+Space")),
        Some("dictation"),
        "the holder keeps the chord"
    );
    registry.bind(&chord("Shift+Cmd+K"), "a").unwrap();
    assert!(
        registry.bind(&chord("Meta+Shift+K"), "b").is_err(),
        "spelling and order do not matter"
    );
    assert!(
        registry.bind(&chord("Ctrl+Space"), "dictation").is_ok(),
        "the same owner may rebind"
    );
    registry.release(&chord("Ctrl+Space"));
    assert_eq!(registry.owner_of(&chord("Ctrl+Space")), None);
    assert!(registry.bind(&chord("Ctrl+Space"), "launcher").is_ok());

    let named = HotkeyConflict::from_registration_error(
        "register Ctrl+Space failed: already taken",
        &["Ctrl+Space", "Alt+Space"],
        "in use",
    );
    assert_eq!(named.shortcuts, ["Ctrl+Space"]);
    assert_eq!(named.message, "in use");
    let unknown = HotkeyConflict::from_registration_error(
        "register F13 failed: nope",
        &["Ctrl+Space", "Alt+Space"],
        "in use",
    );
    assert_eq!(
        unknown.shortcuts,
        ["Ctrl+Space", "Alt+Space"],
        "an unrelated shortcut names every required one"
    );
    assert_eq!(
        HotkeyConflict::from_registration_error("garbage", &["A"], "m").shortcuts,
        ["A"]
    );
}

// ---- hold tracking and registration failures -----------------------------------------------

#[derive(Default)]
struct FakeKeys(RefCell<HashSet<u16>>);

impl FakeKeys {
    fn down(&self, vk: u16) {
        self.0.borrow_mut().insert(vk);
    }
    fn up(&self, vk: u16) {
        self.0.borrow_mut().remove(&vk);
    }
}

impl KeyStateSource for FakeKeys {
    fn is_down(&self, vk: u16) -> bool {
        self.0.borrow().contains(&vk)
    }
}

fn hold_tracking_and_failures() {
    const CTRL: u16 = 0x11;
    const SPACE: u16 = 0x20;
    let ctrl_space = || HoldKeys {
        key: SPACE,
        modifier_groups: vec![vec![CTRL]],
    };
    let keys = FakeKeys::default();
    keys.down(CTRL);
    keys.down(SPACE);
    let mut tracker = HoldTracker::new();
    assert!(tracker.is_idle());
    assert!(tracker.press(1, ctrl_space()));
    assert!(
        !tracker.press(1, ctrl_space()),
        "a repeated press is not a second Pressed"
    );
    assert!(!tracker.is_idle());
    assert!(tracker.poll(&keys).is_empty());
    keys.up(SPACE);
    assert_eq!(tracker.poll(&keys), [1]);
    assert!(tracker.poll(&keys).is_empty(), "exactly one Released");
    assert!(tracker.is_idle());

    keys.down(SPACE);
    tracker.press(2, ctrl_space());
    keys.up(CTRL);
    assert_eq!(
        tracker.poll(&keys),
        [2],
        "releasing the modifier first ends the hold"
    );

    let either_win = HoldKeys {
        key: SPACE,
        modifier_groups: vec![vec![0x5B, 0x5C]],
    };
    keys.down(0x5C);
    tracker.press(3, either_win);
    assert!(
        tracker.poll(&keys).is_empty(),
        "either Win key keeps the group down"
    );

    assert!(tracker.release(3));
    assert!(!tracker.release(3));
    tracker.press(4, ctrl_space());
    tracker.press(5, ctrl_space());
    let mut released = tracker.release_all();
    released.sort();
    assert_eq!(released, [4, 5]);
    assert!(tracker.release_all().is_empty());
    assert_ne!(HotkeyState::Pressed, HotkeyState::Released);

    for (code, failure) in [
        (1409, RegisterFailure::Conflict),
        (87, RegisterFailure::Unsupported),
        (1004, RegisterFailure::Unsupported),
        (5, RegisterFailure::PermissionDenied),
        (8, RegisterFailure::ResourceExhausted),
        (14, RegisterFailure::ResourceExhausted),
        (1450, RegisterFailure::ResourceExhausted),
        (1400, RegisterFailure::Other),
    ] {
        assert_eq!(RegisterFailure::from_win32(code), failure, "win32 {code}");
    }
    for (status, failure) in [
        (-9878, RegisterFailure::Conflict),
        (-9879, RegisterFailure::Unsupported),
        (-50, RegisterFailure::Unsupported),
        (-108, RegisterFailure::ResourceExhausted),
        (-1, RegisterFailure::Other),
    ] {
        assert_eq!(
            RegisterFailure::from_osstatus(status),
            failure,
            "OSStatus {status}"
        );
    }
}

// ---- push-to-talk policy -------------------------------------------------------------------

fn push_to_talk_policy() {
    let cause = PttAbortCause {
        kind: PttAbortKind::ModifierJoined,
        key_code: Some(49),
        modifiers: PttModifiers(PttModifiers::CTRL | PttModifiers::SHIFT),
        joined: PttModifiers(PttModifiers::ALT),
        phase: PttPhase::Starting,
        swallowed: true,
    };
    assert!(
        cause.modifiers.contains(PttModifiers::SHIFT)
            && !cause.modifiers.contains(PttModifiers::FN)
    );
    let line = format_abort("test", cause);
    assert_eq!(
        line,
        "platform=test kind=modifier_joined key_code=49 modifiers=ctrl:true,shift:true,alt:false,command_or_win:false,fn:false joined=ctrl:false,shift:false,alt:true,command_or_win:false,fn:false phase=starting swallowed=true"
    );
    let plain = PttAbortCause {
        kind: PttAbortKind::KeyDown,
        key_code: None,
        modifiers: PttModifiers::default(),
        joined: PttModifiers::default(),
        phase: PttPhase::Arming,
        swallowed: false,
    };
    assert!(format_abort("test", plain).contains("kind=key_down key_code=none"));

    // A release that arrives while the start is still running is applied when the start completes.
    let handoff = PttHandoff::new();
    assert!(!handoff.is_active() && handoff.phase().is_none());
    assert!(handoff.begin_start());
    assert!(!handoff.begin_start(), "one start at a time");
    assert!(handoff.is_active());
    assert_eq!(handoff.phase(), Some(PttPhase::Starting));
    assert_eq!(
        handoff.request_end(PttEnd::Stop, None),
        None,
        "deferred until the start completes"
    );
    assert_eq!(handoff.complete_start(true), Some((PttEnd::Stop, None)));
    assert!(!handoff.is_active());

    // An undisturbed start becomes a recording that the release then ends.
    assert!(handoff.begin_start());
    assert_eq!(handoff.complete_start(true), None);
    assert_eq!(handoff.phase(), Some(PttPhase::Recording));
    assert_eq!(
        handoff.request_end(PttEnd::Send, None),
        Some((PttEnd::Send, None))
    );
    assert_eq!(handoff.phase(), None);
    assert_eq!(
        handoff.request_end(PttEnd::Stop, None),
        None,
        "nothing to end when idle"
    );

    // A start that is no longer valid ends itself with a stop.
    assert!(handoff.begin_start());
    assert_eq!(handoff.complete_start(false), Some((PttEnd::Stop, None)));

    // A cancel carries its cause through to whoever ends the recording; the strongest end wins.
    assert!(handoff.begin_start());
    assert_eq!(handoff.request_end(PttEnd::Stop, None), None);
    assert_eq!(handoff.request_end(PttEnd::Cancel, Some(cause)), None);
    assert_eq!(handoff.request_end(PttEnd::Send, None), None);
    assert_eq!(
        handoff.complete_start(true),
        Some((PttEnd::Cancel, Some(cause)))
    );
    assert!(handoff.begin_start());
    handoff.complete_start(true);
    assert_eq!(
        handoff.request_end(PttEnd::Cancel, Some(cause)),
        Some((PttEnd::Cancel, Some(cause)))
    );
    assert!(handoff.begin_start());
    handoff.reset();
    assert!(!handoff.is_active());

    assert!(should_arm_ptt(true, false, false));
    assert!(!should_arm_ptt(false, false, false));
    assert!(
        !should_arm_ptt(true, true, false),
        "not while dictation is active"
    );
    assert!(
        !should_arm_ptt(true, false, true),
        "not with other modifiers down"
    );
    assert_eq!(PTT_CHORD_ABORT_WINDOW_MS, 1_000);
    assert!(should_abort_ptt_chord(PTT_CHORD_ABORT_WINDOW_MS - 1));
    assert!(!should_abort_ptt_chord(PTT_CHORD_ABORT_WINDOW_MS));
    assert!(should_clear_ptt_hold_suppression(false, false));
    assert!(!should_clear_ptt_hold_suppression(true, false));
    assert!(!should_clear_ptt_hold_suppression(false, true));

    let order = RefCell::new(Vec::new());
    let value = run_feedback_before_action(
        || order.borrow_mut().push("feedback"),
        || {
            order.borrow_mut().push("action");
            7
        },
    );
    assert_eq!((value, order.into_inner()), (7, vec!["feedback", "action"]));

    assert_eq!(MOUSE_BUTTON_MIDDLE, 2);
    assert!(other_mouse_down_is_middle(2));
    for button in [0, 1, 3, 4, 31] {
        assert!(
            !other_mouse_down_is_middle(button),
            "button {button} passes through"
        );
    }
    assert!(rightkit_hotkeys::decision::should_accept_fn_chord(
        true, true, true
    ));
    for (event, cached, enabled) in [
        (false, true, true),
        (true, false, true),
        (true, true, false),
    ] {
        assert!(!rightkit_hotkeys::decision::should_accept_fn_chord(
            event, cached, enabled
        ));
    }
}

// ---- raw-event chord matcher ---------------------------------------------------------------

fn hook_matcher() {
    const LCONTROL: u32 = 0xA2;
    const LMENU: u32 = 0xA4;
    const LSHIFT: u32 = 0xA0;
    const RSHIFT: u32 = 0xA1;
    const SPACE: u32 = 0x20;
    const K: u32 = 0x4B;
    let pass = HookOutcome::default();
    let fired = |id, state| HookOutcome {
        events: vec![(id, state)],
        swallow: true,
    };

    let mut hook = HookChords::new();
    assert!(hook.is_empty());
    let alt_space = hook.bind(&chord("alt+space")).expect("bind");
    let ctrl_shift_k = hook.bind(&chord("ctrl+shift+k")).expect("bind");
    assert_ne!(alt_space, ctrl_shift_k);
    assert!(!hook.is_empty());
    assert_eq!(
        hook.bind(&chord("Space+Alt")),
        Err(AlreadyBound(alt_space)),
        "a chord binds once, whatever the spelling"
    );

    assert_eq!(
        hook.on_key(SPACE, true, false),
        pass,
        "the key alone is not the chord"
    );
    assert_eq!(hook.on_key(SPACE, false, false), pass);
    assert_eq!(
        hook.on_key(LMENU, true, false),
        pass,
        "modifiers are never swallowed"
    );
    assert_eq!(
        hook.on_key(SPACE, true, false),
        fired(alt_space, HotkeyState::Pressed)
    );
    assert_eq!(
        hook.on_key(SPACE, true, false),
        HookOutcome {
            events: vec![],
            swallow: true
        },
        "auto-repeat is swallowed silently"
    );
    assert_eq!(
        hook.on_key(SPACE, false, false),
        fired(alt_space, HotkeyState::Released)
    );
    assert_eq!(hook.on_key(LMENU, false, false), pass);

    // Exactly the chord's modifiers: Alt+Shift+Space is not Alt+Space.
    hook.on_key(LMENU, true, false);
    hook.on_key(LSHIFT, true, false);
    assert_eq!(hook.on_key(SPACE, true, false), pass);
    hook.on_key(SPACE, false, false);
    hook.on_key(LSHIFT, false, false);
    hook.on_key(LMENU, false, false);

    // Injected input passes through and changes nothing.
    hook.on_key(LMENU, true, false);
    assert_eq!(hook.on_key(SPACE, true, true), pass);
    assert_eq!(
        hook.on_key(SPACE, true, false),
        fired(alt_space, HotkeyState::Pressed)
    );
    // Releasing the modifier first ends the hold once; the key-up that follows is not swallowed again.
    assert_eq!(
        hook.on_key(LMENU, false, false),
        HookOutcome {
            events: vec![(alt_space, HotkeyState::Released)],
            swallow: false
        }
    );
    assert_eq!(hook.on_key(SPACE, false, false), pass);

    // Left and right variants are the same modifier; one of two held Shift keys going up keeps it down.
    hook.on_key(LCONTROL, true, false);
    hook.on_key(LSHIFT, true, false);
    hook.on_key(RSHIFT, true, false);
    assert_eq!(
        hook.on_key(K, true, false),
        fired(ctrl_shift_k, HotkeyState::Pressed)
    );
    assert_eq!(
        hook.on_key(LSHIFT, false, false),
        pass,
        "the other Shift key is still down"
    );
    assert_eq!(
        hook.on_key(K, false, false),
        fired(ctrl_shift_k, HotkeyState::Released)
    );
    hook.on_key(K, true, false);
    assert_eq!(hook.release_all(), [ctrl_shift_k]);
    assert!(hook.release_all().is_empty());

    hook.on_key(LCONTROL, true, false);
    hook.on_key(LSHIFT, true, false);
    assert_eq!(
        hook.on_key(K, true, false),
        fired(ctrl_shift_k, HotkeyState::Pressed)
    );
    assert_eq!(
        hook.unbind(&chord("Ctrl+Shift+K")),
        Some((ctrl_shift_k, true)),
        "unbinding a held chord reports it was held"
    );
    assert_eq!(hook.unbind(&chord("Ctrl+Shift+K")), None);
    let rebound = hook.bind(&chord("Ctrl+Shift+K")).expect("rebind");
    assert_ne!(rebound, ctrl_shift_k, "ids are never reused");
}

// ---- the RegisterHotKey bookkeeping, against a fake OS table -------------------------------

#[derive(Default)]
struct FakeOs {
    live: Vec<(i32, u32, u32)>,
    calls: Vec<String>,
    refuse_with: Option<u32>,
    unregister_error: Option<u32>,
}

impl HotkeyApi for FakeOs {
    fn register(&mut self, id: i32, modifiers: u32, vk: u32) -> Result<(), u32> {
        self.calls
            .push(format!("register {id} {modifiers:#x} {vk:#x}"));
        if let Some(code) = self.refuse_with {
            return Err(code);
        }
        let keys = modifiers & !modifier_bits::NOREPEAT;
        if self.live.iter().any(|(_, m, v)| *m == keys && *v == vk) {
            return Err(1409);
        }
        self.live.push((id, keys, vk));
        Ok(())
    }

    fn unregister(&mut self, id: i32) -> Result<(), u32> {
        self.calls.push(format!("unregister {id}"));
        if let Some(code) = self.unregister_error {
            return Err(code);
        }
        match self.live.iter().position(|(live, _, _)| *live == id) {
            Some(position) => {
                self.live.remove(position);
                Ok(())
            }
            None => Err(ERROR_HOTKEY_NOT_REGISTERED),
        }
    }
}

fn registration_table() {
    assert_eq!(
        (
            modifier_bits::ALT,
            modifier_bits::CONTROL,
            modifier_bits::SHIFT,
            modifier_bits::WIN,
            modifier_bits::NOREPEAT
        ),
        (1, 2, 4, 8, 0x4000)
    );
    assert_eq!(ERROR_HOTKEY_NOT_REGISTERED, 1419);
    let ctrl_alt_k = chord("Ctrl+Alt+K");
    assert_eq!(modifier_flags(&ctrl_alt_k, false), 0x4000 | 2 | 1);
    assert_eq!(
        modifier_flags(&ctrl_alt_k, true),
        2 | 1,
        "a repeating registration omits MOD_NOREPEAT"
    );
    assert_eq!(modifier_flags(&chord("Super+Z"), true), modifier_bits::WIN);
    let keys = hold_keys(&chord("Ctrl+Win+Space"));
    assert_eq!(
        keys,
        HoldKeys {
            key: 0x20,
            modifier_groups: vec![vec![0x11], vec![0x5B, 0x5C]]
        }
    );

    let mut os = FakeOs::default();
    let mut table = Registrations::new();
    assert!(table.is_empty() && table.len() == 0);
    let first = table
        .register(&mut os, &ctrl_alt_k, false)
        .expect("register");
    assert_eq!(first, 1);
    assert_eq!(os.calls[0], "register 1 0x4003 0x4b");
    assert_eq!(
        table.id_of(&chord("alt+ctrl+k")),
        Some(first),
        "matched on the canonical spelling"
    );
    assert_eq!(table.hold_keys(first), Some(&hold_keys(&ctrl_alt_k)));
    assert_eq!(
        table.register(&mut os, &chord("Alt+Ctrl+K"), true),
        Err(1409),
        "the OS answers a repeat with a conflict"
    );
    let second = table
        .register(&mut os, &chord("Ctrl+Alt+J"), true)
        .expect("register");
    assert_eq!(second, 2, "a refused registration does not use up an id");
    assert_eq!(table.len(), 2);

    os.refuse_with = Some(5);
    assert_eq!(table.register(&mut os, &chord("Alt+F9"), false), Err(5));
    assert_eq!(
        RegisterFailure::from_win32(5),
        RegisterFailure::PermissionDenied
    );
    os.refuse_with = None;
    assert_eq!(
        table.id_of(&chord("Alt+F9")),
        None,
        "nothing is recorded for a refusal"
    );

    assert_eq!(table.unregister(&mut os, &ctrl_alt_k), Ok(first));
    assert_eq!(
        table.unregister(&mut os, &ctrl_alt_k),
        Err(UnregisterFailure::NotRegistered)
    );
    // The OS forgot a registration behind our back: the stale record is dropped.
    os.live.clear();
    assert_eq!(
        table.unregister(&mut os, &chord("Ctrl+Alt+J")),
        Err(UnregisterFailure::NotRegistered)
    );
    assert!(table.is_empty());
    // The OS refuses to unregister: the record is kept because the hotkey is still live.
    let third = table
        .register(&mut os, &chord("Ctrl+Alt+L"), false)
        .unwrap();
    os.unregister_error = Some(5);
    assert_eq!(
        table.unregister(&mut os, &chord("Ctrl+Alt+L")),
        Err(UnregisterFailure::Os(5))
    );
    assert_eq!(table.id_of(&chord("Ctrl+Alt+L")), Some(third));
    os.unregister_error = None;
    table.clear(&mut os);
    assert!(table.is_empty() && os.live.is_empty());
}

// ---- raw event decoding --------------------------------------------------------------------

fn hook_detail_decoding() {
    let detail = KeyEventDetail::from_raw(0x20, true, 0x39, LLKHF_ALTDOWN, 0xABCD);
    assert_eq!(
        (detail.vk, detail.down, detail.scan_code, detail.extra_info),
        (0x20, true, 0x39, 0xABCD)
    );
    assert!(detail.alt_down && !detail.injected && !detail.extended);
    let detail = KeyEventDetail::from_raw(0x41, false, 0x1E, LLKHF_INJECTED | LLKHF_EXTENDED, 0);
    assert!(detail.injected && detail.extended && !detail.alt_down && !detail.down);
    assert_eq!(
        (
            LLKHF_EXTENDED,
            LLKHF_INJECTED,
            LLKHF_ALTDOWN,
            LLMHF_INJECTED
        ),
        (0x01, 0x10, 0x20, 0x01)
    );

    for (message, data, kind) in [
        (0x0200, 0, MouseKind::Move),
        (0x0201, 0, MouseKind::LeftDown),
        (0x0202, 0, MouseKind::LeftUp),
        (0x0204, 0, MouseKind::RightDown),
        (0x0205, 0, MouseKind::RightUp),
        (0x0207, 0, MouseKind::MiddleDown),
        (0x0208, 0, MouseKind::MiddleUp),
        (0x020A, 0, MouseKind::Wheel),
        (0x020E, 0, MouseKind::HWheel),
        (0x020B, 1 << 16, MouseKind::X1Down),
        (0x020C, 1 << 16, MouseKind::X1Up),
        (0x020B, 2 << 16, MouseKind::X2Down),
        (0x020C, 2 << 16, MouseKind::X2Up),
        (0x020B, 3 << 16, MouseKind::Other(0x020B)),
        (0x0203, 0, MouseKind::Other(0x0203)),
    ] {
        assert_eq!(
            classify_mouse_message(message, data),
            kind,
            "message {message:#x}"
        );
    }
    let up = MouseEvent::from_raw(0x020A, 10, 20, 120u32 << 16, 0, 7);
    assert_eq!(
        (up.kind, up.wheel_delta, up.x, up.y, up.extra_info),
        (MouseKind::Wheel, 120, 10, 20, 7)
    );
    let down = MouseEvent::from_raw(
        0x020A,
        0,
        0,
        ((-120i16) as u16 as u32) << 16,
        LLMHF_INJECTED,
        0,
    );
    assert_eq!(
        down.wheel_delta, -120,
        "the wheel delta is the signed high word"
    );
    assert!(down.injected);
    let click = MouseEvent::from_raw(0x0201, 0, 0, 0xFFFF_0000, 0, 0);
    assert_eq!((click.wheel_delta, click.injected), (0, false));

    assert_eq!((THREAD_MESSAGE_MIN, THREAD_MESSAGE_MAX), (0x8000, 0xBFFF));
    assert!(is_thread_message(0x8000) && is_thread_message(0xBFFF));
    for message in [0x7FFF, 0xC000, 0x0012, 0x0400] {
        assert!(!is_thread_message(message), "{message:#x}");
    }
}

// ---- middle-click gate ---------------------------------------------------------------------

fn middle_click_machinery() {
    let now = Instant::now();
    let gate = MiddleClickGate::new();
    assert!(gate.is_idle());
    let generation = match gate.on_down(now, 1.0, 2.0) {
        DownAction::Lookup { generation, x, y } => {
            assert_eq!((x, y), (1.0, 2.0));
            generation
        }
        DownAction::Pass => panic!("the first press starts a lookup"),
    };
    assert!(!gate.is_idle());
    assert_eq!(
        gate.on_down(now + Duration::from_millis(10), 5.0, 6.0),
        DownAction::Pass,
        "a second press passes through while deciding"
    );
    assert_eq!(
        gate.on_up(),
        UpAction::Swallow,
        "the release of the swallowed press is swallowed"
    );
    assert_eq!(gate.resolve(generation, false), Resolution::Toggle);
    assert!(gate.is_idle(), "the release was already seen");

    let generation = match gate.on_down(now, 3.0, 4.0) {
        DownAction::Lookup { generation, .. } => generation,
        DownAction::Pass => panic!("lookup"),
    };
    assert_eq!(
        gate.resolve(generation, true),
        Resolution::Replay {
            x: 3.0,
            y: 4.0,
            with_up: false
        }
    );
    assert_eq!(
        gate.on_up(),
        UpAction::SwallowAndReplayUp { x: 3.0, y: 4.0 }
    );
    assert_eq!(
        gate.resolve(generation, true),
        Resolution::Ignore,
        "a stale answer is ignored"
    );
    assert_eq!(gate.on_up(), UpAction::Pass);

    // A lookup that hangs is abandoned after DECIDE_STALE and a new press takes over.
    let stale = match gate.on_down(now, 0.0, 0.0) {
        DownAction::Lookup { generation, .. } => generation,
        DownAction::Pass => panic!("lookup"),
    };
    let newer = match gate.on_down(now + DECIDE_STALE, 0.0, 0.0) {
        DownAction::Lookup { generation, .. } => generation,
        DownAction::Pass => panic!("a stale lookup is replaced"),
    };
    assert!(newer > stale);
    assert_eq!(gate.resolve(stale, false), Resolution::Ignore);
    gate.reset();
    assert!(gate.is_idle());

    // The tap callback side: disabled, other buttons and a vanished worker all pass through.
    let enabled = AtomicBool::new(true);
    let diagnostics = Mutex::new(Vec::new());
    let diag =
        |name: &str, detail: &str| diagnostics.lock().unwrap().push(format!("{name} {detail}"));
    let jobs = Mutex::new(Vec::new());
    let submit = |job: Job| {
        jobs.lock().unwrap().push(job);
        true
    };
    enabled.store(false, Ordering::SeqCst);
    assert_eq!(
        handle_other_mouse(
            &enabled,
            &gate,
            MouseEdge::Down,
            2,
            (1.0, 1.0),
            now,
            &submit,
            &diag
        ),
        Verdict::Pass
    );
    enabled.store(true, Ordering::SeqCst);
    assert_eq!(
        handle_other_mouse(
            &enabled,
            &gate,
            MouseEdge::Down,
            4,
            (1.0, 1.0),
            now,
            &submit,
            &diag
        ),
        Verdict::Pass
    );
    assert_eq!(
        diagnostics.lock().unwrap().as_slice(),
        [format!("{DIAG_OTHER_BUTTON} button=4")]
    );
    assert_eq!(
        handle_other_mouse(
            &enabled,
            &gate,
            MouseEdge::Up,
            4,
            (1.0, 1.0),
            now,
            &submit,
            &diag
        ),
        Verdict::Pass
    );
    assert!(jobs.lock().unwrap().is_empty());
    assert_eq!(
        handle_other_mouse(
            &enabled,
            &gate,
            MouseEdge::Down,
            2,
            (7.0, 8.0),
            now,
            &submit,
            &diag
        ),
        Verdict::Swallow
    );
    let job = jobs.lock().unwrap()[0];
    assert!(matches!(job, Job::Lookup { x, y, .. } if (x, y) == (7.0, 8.0)));
    gate.reset();
    let gone = |_: Job| false;
    assert_eq!(
        handle_other_mouse(
            &enabled,
            &gate,
            MouseEdge::Down,
            2,
            (7.0, 8.0),
            now,
            &gone,
            &diag
        ),
        Verdict::Pass,
        "no worker, the click passes through"
    );
    assert!(gate.is_idle());

    // The worker side: a non-link toggles, a link replays the click through the injected poster.
    let toggles = Arc::new(AtomicUsize::new(0));
    let posted = Arc::new(Mutex::new(Vec::new()));
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let toggle_counter = toggles.clone();
    let make_actions = |is_link: bool| {
        let posted = posted.clone();
        let toggle_counter = toggle_counter.clone();
        let done_tx = Mutex::new(done_tx.clone());
        JobActions {
            is_link: Box::new(move |_, _| is_link),
            post: Box::new(move |press, x, y| posted.lock().unwrap().push((press, x, y))),
            toggle: Arc::new(move || {
                toggle_counter.fetch_add(1, Ordering::SeqCst);
                let _ = done_tx.lock().unwrap().send(());
            }),
        }
    };
    let generation = match gate.on_down(now, 7.0, 8.0) {
        DownAction::Lookup { generation, .. } => generation,
        DownAction::Pass => panic!("lookup"),
    };
    run_job(
        &gate,
        Job::Lookup {
            generation,
            x: 7.0,
            y: 8.0,
        },
        &mut make_actions(false),
    );
    done_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the toggle callback runs");
    assert_eq!(toggles.load(Ordering::SeqCst), 1);
    assert!(posted.lock().unwrap().is_empty());
    gate.reset();
    let generation = match gate.on_down(now, 9.0, 10.0) {
        DownAction::Lookup { generation, .. } => generation,
        DownAction::Pass => panic!("lookup"),
    };
    run_job(
        &gate,
        Job::Lookup {
            generation,
            x: 9.0,
            y: 10.0,
        },
        &mut make_actions(true),
    );
    assert_eq!(posted.lock().unwrap().as_slice(), [(true, 9.0, 10.0)]);
    run_job(
        &gate,
        Job::ReplayUp { x: 9.0, y: 10.0 },
        &mut make_actions(true),
    );
    assert_eq!(
        posted.lock().unwrap().as_slice(),
        [(true, 9.0, 10.0), (false, 9.0, 10.0)]
    );
}

// ---- push-to-talk arm timer ----------------------------------------------------------------

fn arm_timer() {
    let spawn = || {
        let (tx, rx) = mpsc::channel();
        let timer = ArmTimer::spawn("consumer-arm-timer", move |generation| {
            let _ = tx.send(generation);
        })
        .expect("spawn the arm timer");
        (timer, rx)
    };
    assert_eq!(MAX_ARM_DELAY, Duration::from_millis(1_000));

    let (timer, rx) = spawn();
    timer.arm(7, Duration::from_millis(20));
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(10)),
        Ok(7),
        "an armed deadline fires"
    );
    assert!(
        rx.recv_timeout(Duration::from_millis(150)).is_err(),
        "and fires once"
    );

    let (timer, rx) = spawn();
    timer.arm(1, Duration::from_millis(100));
    timer.cancel();
    assert!(
        rx.recv_timeout(Duration::from_millis(400)).is_err(),
        "a cancelled deadline never fires"
    );

    let (timer, rx) = spawn();
    for generation in 1..=50 {
        timer.arm(generation, Duration::from_millis(80));
    }
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(10)),
        Ok(50),
        "arming again replaces the pending deadline"
    );
    assert!(rx.recv_timeout(Duration::from_millis(250)).is_err());

    let (timer, rx) = spawn();
    timer.arm(3, Duration::from_millis(100));
    drop(timer);
    assert!(
        rx.recv_timeout(Duration::from_millis(400)).is_err(),
        "dropping the timer stops it without firing"
    );

    let (timer, rx) = spawn();
    let started = Instant::now();
    timer.arm(9, Duration::from_secs(3600));
    assert_eq!(rx.recv_timeout(Duration::from_secs(10)), Ok(9));
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "an hour is clamped to MAX_ARM_DELAY"
    );
}

// ---- platform types: exported, never constructed ------------------------------------------

#[cfg(windows)]
fn platform_surface() {
    use rightkit_hotkeys::windows::{
        GlobalHotkeys, HookConfig, HookDecision, HookError, HookHotkeys, KeyEvent, LowLevelHooks,
        LowLevelKeyboardHook, RegisterError, UnregisterError, LLKHF_INJECTED, RELEASE_POLL_MS,
    };
    assert_eq!(RELEASE_POLL_MS, 15);
    assert_eq!(LLKHF_INJECTED, 0x10);
    let sizes = [
        std::mem::size_of::<GlobalHotkeys>(),
        std::mem::size_of::<HookHotkeys>(),
        std::mem::size_of::<LowLevelKeyboardHook>(),
        std::mem::size_of::<LowLevelHooks>(),
        std::mem::size_of::<HookConfig>(),
        std::mem::size_of::<KeyEvent>(),
        std::mem::size_of::<HookDecision>(),
        std::mem::size_of::<HookError>(),
        std::mem::size_of::<RegisterError>(),
        std::mem::size_of::<UnregisterError>(),
    ];
    assert!(sizes.iter().all(|size| *size > 0));
}

#[cfg(target_os = "macos")]
fn platform_surface() {
    use rightkit_hotkeys::macos::{
        EventTap, FnHotkeyCallbacks, FnHotkeyTap, GlobalHotkeys, HotkeyId, MiddleMouseTap, PttKey,
        RegisterError, TapConfig, TapDecision, TapError, TapEvent, TapKind, TapLocation,
    };
    let sizes = [
        std::mem::size_of::<GlobalHotkeys>(),
        std::mem::size_of::<FnHotkeyTap>(),
        std::mem::size_of::<FnHotkeyCallbacks>(),
        std::mem::size_of::<MiddleMouseTap>(),
        std::mem::size_of::<EventTap>(),
        std::mem::size_of::<HotkeyId>(),
        std::mem::size_of::<PttKey>(),
        std::mem::size_of::<RegisterError>(),
        std::mem::size_of::<TapConfig>(),
        std::mem::size_of::<TapDecision>(),
        std::mem::size_of::<TapError>(),
        std::mem::size_of::<TapEvent>(),
        std::mem::size_of::<TapKind>(),
        std::mem::size_of::<TapLocation>(),
    ];
    assert!(sizes.iter().all(|size| *size > 0));
}

#[cfg(not(any(windows, target_os = "macos")))]
fn platform_surface() {
    // Only the pure modules exist on this platform; the rest of this file already used them all.
}
