//! Windows: beat a [`HangWatchdog`] from a window's message loop. The watchdog posts one message to
//! the window each check interval; the window procedure calls [`run_pending_beat`] when it gets
//! it. While the thread that owns the window does not run its message loop the beat never runs and
//! the hang is reported.
//!
//! One pump per process: the single pending slot is shared.

#![allow(unsafe_code)]

use crate::HangWatchdog;
use std::ffi::c_void;
use std::sync::Mutex;

/// The one beat waiting for the window's thread: the watchdog keeps at most one outstanding, so a
/// single slot is enough.
static PENDING_BEAT: Mutex<Option<Box<dyn FnOnce() + Send>>> = Mutex::new(None);

#[link(name = "user32")]
unsafe extern "system" {
    fn PostMessageW(window: *mut c_void, message: u32, wparam: usize, lparam: isize) -> i32;
}

/// Runs the waiting beat, if any. Call from the window procedure when it receives the message
/// given to [`pump_via_window`]; it returns at once.
pub fn run_pending_beat() {
    let beat = PENDING_BEAT.lock().ok().and_then(|mut slot| slot.take());
    if let Some(beat) = beat {
        beat();
    }
}

/// Starts the beat pump for `watchdog`: every check interval `message` (no parameters) is posted
/// to `window` (an `HWND` as an integer), and the window procedure answers it with
/// [`run_pending_beat`]. `name` is the watched loop's name in the hang report. The pump ends when
/// the post fails, as it does once the window is gone. Errors only when the pump thread cannot
/// start.
pub fn pump_via_window(
    watchdog: &HangWatchdog,
    name: &str,
    window: isize,
    message: u32,
) -> std::io::Result<()> {
    watchdog.pump_via(name, move |beat| {
        let Ok(mut slot) = PENDING_BEAT.lock() else {
            return false;
        };
        *slot = Some(beat);
        drop(slot);
        // SAFETY: PostMessageW tolerates a stale handle (it fails once the window is gone, which
        // ends the pump); the parameters are plain integers.
        let posted = unsafe { PostMessageW(window as *mut c_void, message, 0, 0) } != 0;
        if !posted {
            if let Ok(mut slot) = PENDING_BEAT.lock() {
                *slot = None;
            }
        }
        posted
    })
}
