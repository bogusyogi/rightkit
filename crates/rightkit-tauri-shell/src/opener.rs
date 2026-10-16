//! System handler for external links. Arguments are passed as argv (never through a shell) and
//! only schemes allowed by [`crate::nav::external_scheme_allowed`] reach it.

use std::process::{Command, Stdio};

use crate::nav::external_scheme_allowed;

pub trait ExternalOpener: Send + Sync + 'static {
    /// Open `target` (already scheme-validated) with the operating system's handler.
    fn open(&self, target: &str) -> Result<(), String>;
}

/// The platform handler: `open` (macOS), `rundll32 url.dll,FileProtocolHandler` (Windows),
/// `xdg-open` (Linux).
pub struct SystemOpener;

impl ExternalOpener for SystemOpener {
    fn open(&self, target: &str) -> Result<(), String> {
        let mut cmd;
        #[cfg(target_os = "macos")]
        {
            cmd = Command::new("/usr/bin/open");
            cmd.arg(target);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd = Command::new("rundll32.exe");
            cmd.arg("url.dll,FileProtocolHandler").arg(target);
            // CREATE_NO_WINDOW: never flash a console for background automation.
            cmd.creation_flags(0x0800_0000);
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            cmd = Command::new("xdg-open");
            cmd.arg(target);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd.spawn()
            .map(|mut child| {
                // Reap without blocking the caller.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            })
            .map_err(|e| format!("open {target}: {e}"))
    }
}

/// Validate and open. Refuses anything outside http(s)/mailto.
pub fn open_checked(opener: &dyn ExternalOpener, target: &str) -> Result<(), String> {
    if !external_scheme_allowed(target) {
        return Err("blocked: only http(s)/mailto links open externally".into());
    }
    opener.open(target.trim())
}
