//! Audio is always muted during QA. Apps are told through `RIGHTKIT_QA_MUTE=1`
//! and Chrome gets `--mute-audio`; on macOS the system output can additionally
//! be muted for the run and restored afterwards (opt-in, restored on drop).
//! Playback is asserted from DOM/engine state, never from sound.
use std::process::{Command, Stdio};

pub struct SystemMute {
    restore: Option<bool>,
}

impl SystemMute {
    /// Mute system output (macOS only). A no-op elsewhere; `active()` reports it.
    pub fn engage() -> Self {
        #[cfg(target_os = "macos")]
        {
            let was = Command::new("osascript")
                .args(["-e", "output muted of (get volume settings)"])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "true");
            if let Some(was_muted) = was {
                if !was_muted {
                    let _ = Command::new("osascript").args(["-e", "set volume with output muted"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
                }
                return Self { restore: Some(was_muted) };
            }
        }
        Self { restore: None }
    }
    pub fn active(&self) -> bool {
        self.restore.is_some()
    }
}

impl Drop for SystemMute {
    fn drop(&mut self) {
        if self.restore == Some(false) {
            let _ = Command::new("osascript").args(["-e", "set volume without output muted"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
        }
    }
}
