//! Idle window authority: an explicit value always beats the environment. Own binary because the
//! environment is process-global.
#![cfg(feature = "client")]

use rightkit_tts::worker_args::{env_idle_timeout, parse, WorkerArgs};
use std::sync::Mutex;
use std::time::Duration;

static LOCK: Mutex<()> = Mutex::new(());

fn with_env<R>(rightkit: Option<&str>, genright: Option<&str>, f: impl FnOnce() -> R) -> R {
    let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    for (name, value) in [
        ("RIGHTKIT_TTS_IDLE_SECS", rightkit),
        ("GENRIGHT_QTS_IDLE_SECS", genright),
    ] {
        match value {
            Some(v) => std::env::set_var(name, v),
            None => std::env::remove_var(name),
        }
    }
    let out = f();
    std::env::remove_var("RIGHTKIT_TTS_IDLE_SECS");
    std::env::remove_var("GENRIGHT_QTS_IDLE_SECS");
    out
}

fn explicit(secs: &str) -> WorkerArgs {
    parse(["--idle-secs".to_string(), secs.to_string()]).unwrap()
}

#[test]
fn an_explicit_window_wins_over_both_environment_variables() {
    with_env(Some("5"), Some("9"), || {
        assert_eq!(explicit("42").idle_timeout(), Duration::from_secs(42));
        // Even a smaller or zero explicit value beats a larger ambient one.
        assert_eq!(explicit("0").idle_timeout(), Duration::ZERO);
    });
}

#[test]
fn nothing_explicit_keeps_todays_environment_behaviour() {
    let none = WorkerArgs::default();
    with_env(None, None, || {
        assert_eq!(none.idle_timeout(), Duration::from_secs(600));
        assert_eq!(env_idle_timeout(), Duration::from_secs(600));
    });
    with_env(Some("5"), Some("9"), || {
        assert_eq!(
            none.idle_timeout(),
            Duration::from_secs(5),
            "RIGHTKIT first"
        );
    });
    with_env(None, Some("9"), || {
        assert_eq!(
            none.idle_timeout(),
            Duration::from_secs(9),
            "legacy name still read"
        );
    });
    with_env(Some("0"), None, || {
        assert_eq!(none.idle_timeout(), Duration::ZERO, "0 stays a zero window");
    });
    with_env(Some("soon"), None, || {
        assert_eq!(
            none.idle_timeout(),
            Duration::from_secs(600),
            "unparsable counts as unset"
        );
    });
}
