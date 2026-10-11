//! The host's say over window visibility. The Tauri mock runtime cannot host `ShellState` (it
//! holds `Wry` types), so every path is driven through the same seam production uses:
//! `ShowPolicy::mode` (the decision behind `lifecycle::reveal_mode`) and `windows::apply_reveal`
//! with a recording `RevealTarget`. A forbidden reveal must record zero calls.

use std::ffi::OsString;

use rightkit_shell::lifecycle::{
    env_means_never_show, reopen_wants_reveal, RevealMode, RevealSource, ShowPolicy,
};
use rightkit_shell::windows::{apply_reveal, RevealStyle, RevealTarget};

const ALL_SOURCES: [RevealSource; 9] = [
    RevealSource::Reopen,
    RevealSource::Opened,
    RevealSource::SecondInstance,
    RevealSource::ShowOnReady,
    RevealSource::Tray,
    RevealSource::WindowCommand,
    RevealSource::OpenWindow,
    RevealSource::Restore,
    RevealSource::Api,
];

const ALL_STYLES: [RevealStyle; 3] = [
    RevealStyle::Full,
    RevealStyle::ShowFocus,
    RevealStyle::ShowOnly,
];

#[derive(Default)]
struct Recorder {
    calls: Vec<&'static str>,
    order_back_fails: bool,
}

impl RevealTarget for Recorder {
    fn prepare(&mut self) {
        self.calls.push("prepare");
    }
    fn app_show(&mut self) {
        self.calls.push("app_show");
    }
    fn unminimize(&mut self) -> Result<(), String> {
        self.calls.push("unminimize");
        Ok(())
    }
    fn show(&mut self) -> Result<(), String> {
        self.calls.push("show");
        Ok(())
    }
    fn focus(&mut self) -> Result<(), String> {
        self.calls.push("focus");
        Ok(())
    }
    fn order_back(&mut self) -> Result<(), String> {
        self.calls.push("order_back");
        if self.order_back_fails {
            Err("no native ordering".into())
        } else {
            Ok(())
        }
    }
}

fn no_env(_: &str) -> Option<OsString> {
    None
}

fn env_with(name: &'static str, value: &'static str) -> impl Fn(&str) -> Option<OsString> {
    move |n: &str| (n == name).then(|| OsString::from(value))
}

/// The style each production caller uses for a source.
fn style_for(source: RevealSource) -> RevealStyle {
    match source {
        RevealSource::ShowOnReady => RevealStyle::ShowOnly,
        RevealSource::WindowCommand => RevealStyle::ShowFocus,
        _ => RevealStyle::Full,
    }
}

fn run(policy: &ShowPolicy, source: RevealSource, qa_background: bool) -> (RevealMode, Recorder) {
    let mode = policy.mode(source, qa_background, &no_env);
    let mut recorder = Recorder::default();
    let _ = apply_reveal(mode, style_for(source), &mut recorder);
    (mode, recorder)
}

#[test]
fn the_default_policy_changes_nothing() {
    let policy = ShowPolicy::default();
    for source in ALL_SOURCES {
        assert_eq!(
            policy.mode(source, false, &no_env),
            RevealMode::Foreground,
            "{source:?}"
        );
        assert_eq!(
            policy.mode(source, true, &no_env),
            RevealMode::Background,
            "{source:?}"
        );
    }
}

#[test]
fn never_show_makes_zero_window_calls_for_every_source_style_and_qa_mode() {
    let policy = ShowPolicy {
        never_show: true,
        ..ShowPolicy::default()
    };
    for source in ALL_SOURCES {
        for qa_background in [false, true] {
            assert_eq!(
                policy.mode(source, qa_background, &no_env),
                RevealMode::Forbidden
            );
            for style in ALL_STYLES {
                let mut recorder = Recorder::default();
                let shown = apply_reveal(RevealMode::Forbidden, style, &mut recorder);
                assert_eq!(shown, Ok(false));
                assert!(recorder.calls.is_empty(), "{source:?} {style:?}");
            }
            let (_, recorder) = run(&policy, source, qa_background);
            assert!(recorder.calls.is_empty(), "{source:?}");
        }
    }
}

#[test]
fn never_show_survives_reopen_opened_second_instance_and_ready_events() {
    let policy = ShowPolicy {
        never_show: true,
        ..ShowPolicy::default()
    };
    // A Dock reopen with no visible window asks to reveal; the guard refuses.
    assert!(reopen_wants_reveal(false));
    for source in [
        RevealSource::Reopen,
        RevealSource::Opened,
        RevealSource::SecondInstance,
        RevealSource::ShowOnReady,
    ] {
        let (mode, recorder) = run(&policy, source, false);
        assert_eq!(mode, RevealMode::Forbidden, "{source:?}");
        assert_eq!(recorder.calls, Vec::<&str>::new(), "{source:?}");
    }
}

#[test]
fn the_registered_environment_variable_means_never_show() {
    let policy = ShowPolicy {
        never_show_env: vec!["RIGHTKIT_QA_NATIVE_REAL".into()],
        ..ShowPolicy::default()
    };
    let set = env_with("RIGHTKIT_QA_NATIVE_REAL", "1");
    for source in ALL_SOURCES {
        assert_eq!(policy.mode(source, false, &set), RevealMode::Forbidden);
        assert_eq!(policy.mode(source, true, &set), RevealMode::Forbidden);
    }
    // Unset, or set to a false-ish value, or a different variable: not never-show.
    assert_eq!(
        policy.mode(RevealSource::Tray, false, &no_env),
        RevealMode::Foreground
    );
    for off in ["", "0", "false", "OFF", " no ", "native", " 1"] {
        let env = env_with("RIGHTKIT_QA_NATIVE_REAL", off);
        assert_eq!(
            policy.mode(RevealSource::Tray, false, &env),
            RevealMode::Foreground,
            "{off:?}"
        );
    }
    let other = env_with("SOMETHING_ELSE", "1");
    assert_eq!(
        policy.mode(RevealSource::Tray, false, &other),
        RevealMode::Foreground
    );
    // Several names: any one is enough.
    let two = ShowPolicy {
        never_show_env: vec!["A_ONE".into(), "A_TWO".into()],
        ..ShowPolicy::default()
    };
    assert!(two.is_never_show(&env_with("A_TWO", "yes")));
    assert!(!two.is_never_show(&no_env));
}

#[test]
fn env_values_that_mean_never_show() {
    use std::ffi::OsStr;
    assert!(!env_means_never_show(None));
    // Exact match against the documented truthy values only; everything else is off.
    for no in [
        "", "  ", "0", "false", "False", "no", "off", "native", "2", " 1", "1 ", " no ", "y", "t",
        "enabled", "truee",
    ] {
        assert!(!env_means_never_show(Some(OsStr::new(no))), "{no:?}");
    }
    for yes in ["1", "true", "TRUE", "True", "yes", "Yes", "on", "ON"] {
        assert!(env_means_never_show(Some(OsStr::new(yes))), "{yes:?}");
    }
}

#[test]
fn a_registered_name_with_an_explicit_expected_value_matches_only_that_value() {
    use rightkit_shell::lifecycle::env_equals;
    use std::ffi::OsStr;
    assert!(env_equals(Some(OsStr::new("native")), "native"));
    assert!(!env_equals(Some(OsStr::new("Native")), "native"));
    assert!(!env_equals(Some(OsStr::new("native ")), "native"));
    assert!(!env_equals(Some(OsStr::new("1")), "native"));
    assert!(!env_equals(None, "native"));
    let policy = ShowPolicy {
        never_show_env_values: vec![("QA_MODE".into(), "native".into())],
        ..ShowPolicy::default()
    };
    assert!(policy.is_never_show(&env_with("QA_MODE", "native")));
    // A truthy token is not the expected value, so it is off for this name.
    assert!(!policy.is_never_show(&env_with("QA_MODE", "1")));
    assert!(!policy.is_never_show(&env_with("QA_MODE", "")));
    assert!(!policy.is_never_show(&no_env));
    assert!(!policy.is_never_show(&env_with("OTHER", "native")));
}
