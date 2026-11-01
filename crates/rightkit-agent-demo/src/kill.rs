//! The kill hotkey. One global chord, registered while a session is armed or recording and released
//! when it ends. A press runs `DemoHost::kill`, which engages the process-wide kill switch first.
//!
//! The kill is not an agent tool and has no wire method. Registration failing (the chord is taken)
//! refuses the session: an agent never runs without a working kill control.
//!
//! macOS Carbon hotkeys must be installed on the main thread, so the handle lives in a main-thread
//! `thread_local`; Windows keeps it in a static guarded by a mutex (its loop has its own thread).

use std::sync::Arc;

use rightkit_hotkeys::chord::{parse_chord, Platform};
use tauri::AppHandle;

use super::host::DemoHost;

/// Ctrl+Alt+Shift+Escape. Windows reserves Ctrl+Shift+Escape for Task Manager; adding Alt avoids it.
pub const CHORD: &str = "ctrl+alt+shift+escape";

/// The chord as the person sees it on this OS.
pub fn display() -> String {
    parse_chord(CHORD)
        .map(|c| c.format(Platform::current()))
        .unwrap_or_else(|_| CHORD.to_string())
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;
    use rightkit_hotkeys::macos::GlobalHotkeys;
    use rightkit_hotkeys::HotkeyState;
    use std::cell::RefCell;
    use std::time::Duration;

    thread_local! {
        static HELD: RefCell<Option<GlobalHotkeys>> = const { RefCell::new(None) };
    }

    pub fn arm(app: &AppHandle, host: Arc<DemoHost>) -> Result<(), String> {
        let (tx, rx) = std::sync::mpsc::channel();
        app.run_on_main_thread(move || {
            let result = (|| {
                let chord = parse_chord(CHORD).map_err(|e| e.to_string())?;
                let mut hk = GlobalHotkeys::new(move |_id, state| {
                    if state == HotkeyState::Pressed {
                        host.kill("hotkey");
                    }
                })
                .map_err(|status| {
                    format!("the hotkey dispatcher could not be installed (OSStatus {status})")
                })?;
                hk.register(&chord)
                    .map_err(|e| format!("the kill hotkey could not be registered: {e:?}"))?;
                HELD.with(|h| *h.borrow_mut() = Some(hk));
                Ok::<(), String>(())
            })();
            let _ = tx.send(result);
        })
        .map_err(|e| e.to_string())?;
        rx.recv_timeout(Duration::from_secs(5))
            .map_err(|_| "the main thread did not register the kill hotkey in time".to_string())?
    }

    pub fn disarm(app: &AppHandle) {
        let _ = app.run_on_main_thread(|| {
            HELD.with(|h| *h.borrow_mut() = None);
        });
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use rightkit_hotkeys::windows::GlobalHotkeys;
    use std::sync::Mutex;

    static HELD: Mutex<Option<GlobalHotkeys>> = Mutex::new(None);

    pub fn arm(_app: &AppHandle, host: Arc<DemoHost>) -> Result<(), String> {
        let chord = parse_chord(CHORD).map_err(|e| e.to_string())?;
        let hk = GlobalHotkeys::new(move |_id| host.kill("hotkey"))
            .map_err(|e| format!("the hotkey loop could not start: {e}"))?;
        hk.register(&chord)
            .map_err(|e| format!("the kill hotkey could not be registered: {e:?}"))?;
        *HELD.lock().unwrap() = Some(hk);
        Ok(())
    }

    pub fn disarm(_app: &AppHandle) {
        *HELD.lock().unwrap() = None;
    }
}

/// Registers the chord. Replaces a stale registration. `Err` carries the reason for the person.
pub fn arm(app: &AppHandle, host: &Arc<DemoHost>) -> Result<(), String> {
    imp::disarm(app);
    imp::arm(app, host.clone())
}

pub fn disarm(app: &AppHandle) {
    imp::disarm(app);
}
