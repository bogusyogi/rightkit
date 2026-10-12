//! A command sent the moment `GlobalHotkeys` is created reaches its loop thread.
//!
//! The loop's message queue used to be created by its first `GetMessageW`, after it had handed
//! out its thread id; a wake posted in that gap was lost and the caller waited in `recv` for
//! ever. `unregister` of a chord never registered goes through the same command and wake path
//! without registering anything with Windows.
#![cfg(windows)]

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rightkit_hotkeys::parse_chord;
use rightkit_hotkeys::windows::{GlobalHotkeys, UnregisterError};

#[test]
fn a_command_sent_at_once_after_start_is_answered() {
    let (done_tx, done_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let chord = parse_chord("ctrl+alt+shift+f20").unwrap();
        for _ in 0..300 {
            let hotkeys = GlobalHotkeys::new(|_| {}).unwrap();
            let result = hotkeys.unregister(&chord);
            assert!(
                matches!(result, Err(UnregisterError::NotRegistered { .. })),
                "{result:?}"
            );
        }
        let _ = done_tx.send(());
    });
    match done_rx.recv_timeout(Duration::from_secs(120)) {
        Ok(()) => {}
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("a GlobalHotkeys loop never answered a command sent right after it started")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            worker.join().expect("worker thread");
        }
    }
}
