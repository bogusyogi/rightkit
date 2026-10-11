//! The host's say over window visibility. The Tauri mock runtime cannot host `ShellState` (it
//! holds `Wry` types), so every path is driven through the same seam production uses:
//! `ShowPolicy::mode` (the decision behind `lifecycle::reveal_mode`) and `windows::apply_reveal`
//! with a recording `RevealTarget`. A forbidden reveal must record zero calls.

use std::ffi::OsString;

use rightkit_shell::lifecycle::{
    env_means_never_show, reopen_wants_reveal, RevealMode, RevealSource, ShowPolicy,
};
use rightkit_shell::window_state::RestoreOptions;
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
    for off in ["", "0", "false", "OFF", " no "] {
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
    for no in ["", "  ", "0", "false", "False", "no", "off"] {
        assert!(!env_means_never_show(Some(OsStr::new(no))), "{no:?}");
    }
    for yes in ["1", "true", "yes", "on", "native"] {
        assert!(env_means_never_show(Some(OsStr::new(yes))), "{yes:?}");
    }
}

#[test]
fn the_reopen_opt_out_alone_blocks_only_reopen_and_opened() {
    let policy = ShowPolicy {
        reveal_on_reopen: false,
        ..ShowPolicy::default()
    };
    for qa_background in [false, true] {
        for source in [RevealSource::Reopen, RevealSource::Opened] {
            let (mode, recorder) = run(&policy, source, qa_background);
            assert_eq!(mode, RevealMode::Forbidden, "{source:?}");
            assert!(recorder.calls.is_empty(), "{source:?}");
        }
        for source in [
            RevealSource::SecondInstance,
            RevealSource::ShowOnReady,
            RevealSource::Tray,
            RevealSource::WindowCommand,
            RevealSource::OpenWindow,
            RevealSource::Restore,
            RevealSource::Api,
        ] {
            let (mode, recorder) = run(&policy, source, qa_background);
            assert_ne!(mode, RevealMode::Forbidden, "{source:?}");
            assert!(!recorder.calls.is_empty(), "{source:?}");
        }
    }
}

#[test]
fn foreground_calls_keep_their_order_and_background_only_orders_back() {
    let mut full = Recorder::default();
    assert_eq!(
        apply_reveal(RevealMode::Foreground, RevealStyle::Full, &mut full),
        Ok(true)
    );
    assert_eq!(
        full.calls,
        ["prepare", "app_show", "unminimize", "show", "focus"]
    );

    let mut command = Recorder::default();
    let _ = apply_reveal(RevealMode::Foreground, RevealStyle::ShowFocus, &mut command);
    assert_eq!(command.calls, ["prepare", "show", "focus"]);

    let mut ready = Recorder::default();
    let _ = apply_reveal(RevealMode::Foreground, RevealStyle::ShowOnly, &mut ready);
    assert_eq!(ready.calls, ["show"]);

    for style in ALL_STYLES {
        let mut qa = Recorder::default();
        assert_eq!(
            apply_reveal(RevealMode::Background, style, &mut qa),
            Ok(true)
        );
        assert_eq!(
            qa.calls,
            ["order_back"],
            "{style:?}: never unminimise or focus"
        );
    }
}

#[test]
fn a_failed_order_back_is_silent_for_full_and_an_error_for_the_others() {
    let failing = || Recorder {
        order_back_fails: true,
        ..Recorder::default()
    };
    assert_eq!(
        apply_reveal(RevealMode::Background, RevealStyle::Full, &mut failing()),
        Ok(false)
    );
    assert!(apply_reveal(
        RevealMode::Background,
        RevealStyle::ShowOnly,
        &mut failing()
    )
    .is_err());
    assert!(apply_reveal(
        RevealMode::Background,
        RevealStyle::ShowFocus,
        &mut failing()
    )
    .is_err());
}

#[test]
fn restore_options_are_constrained_by_the_guard() {
    let all = RestoreOptions {
        unminimize: true,
        show: true,
        focus: true,
        restore_minimized: true,
    };
    assert_eq!(all.constrained(RevealMode::Foreground), all);
    let background = all.constrained(RevealMode::Background);
    assert!(!background.unminimize && !background.focus);
    assert!(background.show);
    assert_eq!(
        all.constrained(RevealMode::Forbidden),
        RestoreOptions::default()
    );
}
