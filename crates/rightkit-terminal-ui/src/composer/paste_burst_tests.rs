//! Raw paste tabs remain draft text until the user explicitly submits.
//!
//! Donor `chat_composer/paste_burst_tests.rs` @ `444da310`, adapted to the
//! `Composer`/`Intent` boundary: donor `InputResult::Queued` collapses to
//! `InputResult::Submitted` here because queueing is host-owned — the emitted
//! [`Intent::Submit`] is what the host queues while streaming.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::paste_burst::PasteBurst;
use super::popup_state::ActivePopup;
use super::tests::new_test_composer;
use super::{InputResult, Intent};
use pretty_assertions::assert_eq;

#[test]
fn paste_burst_tabs_preserve_multiline_draft() {
    for running in [false, true] {
        for first_line in ["x", "?", "first line\n"] {
            let (mut composer, _rx) = new_test_composer();
            composer.set_streaming(running);
            let mut now = Instant::now();
            let payload = format!("{first_line}\t\tsecond line\n\tthird line\n");

            for ch in payload.chars() {
                let (result, _) = match ch {
                    '\t' => {
                        assert!(composer.handle_paste_tab(KeyEvent::from(KeyCode::Tab), now));
                        (InputResult::None, true)
                    }
                    '\n' => {
                        composer.handle_submission_with_time(/*should_queue*/ false, now)
                    }
                    ch => composer
                        .handle_input_basic_with_time(KeyEvent::from(KeyCode::Char(ch)), now),
                };
                assert_eq!(result, InputResult::None);
                now += Duration::from_millis(/*millis*/ 1);
            }

            composer.handle_paste_burst_flush(now + PasteBurst::recommended_active_flush_delay());
            assert_eq!(composer.current_text(), payload);

            // Tab pushes while a turn runs; idle, Enter is the send key.
            let key = if running {
                KeyCode::Tab
            } else {
                KeyCode::Enter
            };
            let (result, _) = composer.handle_key_event(KeyEvent::from(key));
            assert_eq!(result, InputResult::Submitted);
            let text = payload.trim().to_string();
            assert_eq!(
                composer.drain_intents(),
                vec![if running {
                    Intent::Push(text)
                } else {
                    Intent::Submit(text)
                }]
            );
        }
    }
}

#[test]
fn paste_burst_tabs_refresh_idle_timeout() {
    for first_char in ['x', '?'] {
        let (mut composer, _rx) = new_test_composer();
        let mut now = Instant::now();
        composer.handle_input_basic_with_time(KeyEvent::from(KeyCode::Char(first_char)), now);
        assert!(composer.handle_paste_tab(KeyEvent::from(KeyCode::Tab), now));

        for _ in 0..3 {
            now += PasteBurst::recommended_active_flush_delay() / 2;
            assert!(composer.handle_paste_tab(KeyEvent::from(KeyCode::Tab), now));
        }

        composer.handle_paste_burst_flush(now + PasteBurst::recommended_active_flush_delay());
        assert_eq!(composer.current_text(), format!("{first_char}\t\t\t\t"));
        assert!(!composer.handle_paste_tab(
            KeyEvent::from(KeyCode::Tab),
            now + PasteBurst::recommended_active_flush_delay()
        ));
    }
}

#[test]
fn paste_burst_tab_does_not_accept_a_completion() {
    let (mut composer, _rx) = new_test_composer();
    composer.insert_str("/");
    composer.sync_popups();
    assert!(matches!(composer.popups.active, ActivePopup::Command(_)));
    composer
        .draft
        .paste_burst
        .begin_with_retro_grabbed("review this".to_string(), Instant::now());

    let (result, _) = composer.handle_key_event(KeyEvent::from(KeyCode::Tab));

    assert_eq!(result, InputResult::None);
    let pasted = composer
        .draft
        .paste_burst
        .flush_before_modified_input()
        .unwrap();
    composer.handle_paste(pasted);
    assert_eq!(composer.current_text(), "/review this\t");
}

#[test]
fn paste_burst_expired_before_tab_still_queues() {
    let (mut composer, _rx) = new_test_composer();
    composer.set_streaming(/*running*/ true);
    composer.handle_input_basic_with_time(
        KeyEvent::from(KeyCode::Char('x')),
        Instant::now() - Duration::from_secs(/*secs*/ 1),
    );

    let (result, _) = composer.handle_key_event(KeyEvent::from(KeyCode::Tab));

    assert_eq!(result, InputResult::Submitted);
    assert_eq!(
        composer.drain_intents(),
        vec![Intent::Push("x".to_string())]
    );
}

#[test]
fn paste_burst_modified_queue_binding_still_dispatches() {
    let (mut composer, _rx) = new_test_composer();
    composer.set_streaming(/*running*/ true);
    composer.composer_keymap.queue = vec![super::key_hint::ctrl(KeyCode::Char('q'))];
    composer.handle_input_basic_with_time(KeyEvent::from(KeyCode::Char('x')), Instant::now());

    let (result, _) =
        composer.handle_key_event(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));

    assert_eq!(result, InputResult::Submitted);
    assert_eq!(
        composer.drain_intents(),
        vec![Intent::Push("x".to_string())]
    );
}
