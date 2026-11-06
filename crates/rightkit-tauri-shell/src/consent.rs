//! Native three-choice consent for a non-web link scheme (#640): Deny, Allow once, Always allow.
//!
//! A page asks the shell to open a link such as `zoom:` or `ms-settings:`. [`crate::nav`] answers
//! `WindowOpenDecision::ProtocolConsent`; this module is the surface that confirms it. The wording
//! stays the app's: [`ConsentPrompt`] carries every string, including the three button labels, so
//! the shell ships no consent vocabulary. "Always allow" is remembered through the app's
//! [`GrantStore`], keyed by the lowercase scheme.
//!
//! The dialog is the platform's own, with the app's labels on the buttons:
//!
//! * Windows: `TaskDialogIndirect` (Common Controls v6). `MessageBoxW` offers only fixed
//!   Yes/No/Cancel labels, so it cannot carry them. The v6 side-by-side dependency is the one every
//!   Tauri window stack already needs (tao, wry and muda import this same entry point); an app gets it
//!   from `tauri-build`, and this crate's tests and benches embed it in `build.rs`.
//! * macOS: `display alert` through `osascript`, the title, text and labels passed as argv so no
//!   text reaches the script source.
//! * Elsewhere: [`ShellError::Unsupported`]; a caller treats any error as Deny.
//!
//! Closing the dialog (Esc, the close box) is Deny, and Deny is the default button.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Mutex;

use tauri::Url;

use crate::nav::origin_of;
use crate::ShellError;

/// What the person chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentChoice {
    Deny,
    AllowOnce,
    AlwaysAllow,
}

/// The app's words for one consent dialog. Every field is app input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentPrompt {
    /// Window title.
    pub title: String,
    /// The question, shown as the dialog's heading.
    pub heading: String,
    /// The explanation under the heading. May be empty.
    pub detail: String,
    pub deny_label: String,
    pub allow_once_label: String,
    pub always_allow_label: String,
    /// Windows: the raw `HWND` that owns the dialog. `None` shows it unowned. Ignored elsewhere.
    pub parent_window: Option<isize>,
}

impl ConsentPrompt {
    pub fn new(title: &str, heading: &str, detail: &str, labels: (&str, &str, &str)) -> Self {
        Self {
            title: title.into(),
            heading: heading.into(),
            detail: detail.into(),
            deny_label: labels.0.into(),
            allow_once_label: labels.1.into(),
            always_allow_label: labels.2.into(),
            parent_window: None,
        }
    }

    /// Refuse a prompt the dialog cannot show unambiguously: an empty or repeated button label
    /// (the macOS answer is read back by label), or a NUL in any text.
    pub fn validate(&self) -> Result<(), ShellError> {
        let labels = [
            &self.deny_label,
            &self.allow_once_label,
            &self.always_allow_label,
        ];
        if labels.iter().any(|l| l.trim().is_empty()) {
            return Err(ShellError::Invalid("consent button label is empty".into()));
        }
        if labels[0] == labels[1] || labels[0] == labels[2] || labels[1] == labels[2] {
            return Err(ShellError::Invalid(
                "consent button labels must differ".into(),
            ));
        }
        let texts = [&self.title, &self.heading, &self.detail];
        if texts.iter().chain(labels.iter()).any(|t| t.contains('\0')) {
            return Err(ShellError::Invalid("consent text contains NUL".into()));
        }
        Ok(())
    }
}

/// Where "Always allow" is remembered. The app decides the storage (its settings, a file); the
/// shell calls it with the lowercase scheme.
pub trait GrantStore: Send + Sync + 'static {
    fn is_granted(&self, scheme: &str) -> Result<bool, ShellError>;
    fn grant(&self, scheme: &str) -> Result<(), ShellError>;
    /// Forget a grant (an app's "reset link permissions" action). Absent is not an error.
    fn revoke(&self, scheme: &str) -> Result<(), ShellError>;
}

/// A [`GrantStore`] kept as a JSON array of schemes in one file, rewritten atomically (temp file in
/// the same folder, then rename) on every change.
pub struct FileGrantStore {
    path: PathBuf,
    schemes: Mutex<BTreeSet<String>>,
}

impl FileGrantStore {
    /// Load `path`. A missing file is an empty store; an unreadable or malformed one is an error
    /// (it is never silently replaced, which would drop the person's grants).
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, ShellError> {
        let path = path.into();
        let schemes = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<BTreeSet<String>>(&bytes).map_err(|e| {
                ShellError::Encoding(format!("consent grants {}: {e}", path.display()))
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
            Err(e) => {
                return Err(ShellError::Io(format!(
                    "read consent grants {}: {e}",
                    path.display()
                )))
            }
        };
        Ok(Self {
            path,
            schemes: Mutex::new(schemes),
        })
    }

    fn persist(&self, schemes: &BTreeSet<String>) -> Result<(), ShellError> {
        let bytes = serde_json::to_vec_pretty(schemes)
            .map_err(|e| ShellError::Encoding(format!("consent grants: {e}")))?;
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)
                .map_err(|e| ShellError::Io(format!("create {}: {e}", dir.display())))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, bytes)
            .map_err(|e| ShellError::Io(format!("write {}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| ShellError::Io(format!("publish {}: {e}", self.path.display())))
    }
}

impl GrantStore for FileGrantStore {
    fn is_granted(&self, scheme: &str) -> Result<bool, ShellError> {
        let schemes = self.schemes.lock().unwrap_or_else(|e| e.into_inner());
        Ok(schemes.contains(&scheme.to_ascii_lowercase()))
    }

    fn grant(&self, scheme: &str) -> Result<(), ShellError> {
        let mut schemes = self.schemes.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = schemes.clone();
        next.insert(scheme.to_ascii_lowercase());
        self.persist(&next)?;
        *schemes = next;
        Ok(())
    }

    fn revoke(&self, scheme: &str) -> Result<(), ShellError> {
        let mut schemes = self.schemes.lock().unwrap_or_else(|e| e.into_inner());
        let mut next = schemes.clone();
        if next.remove(&scheme.to_ascii_lowercase()) {
            self.persist(&next)?;
            *schemes = next;
        }
        Ok(())
    }
}

/// Which page may ask the person for consent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentSurface {
    /// Show the native dialog.
    Prompt,
    /// Refuse without asking: the request came from a page that is not the app's own.
    Refuse,
}

/// Decide whether a page may put a consent dialog in front of the person. With a pinned server
/// origin (`scheme://host[:port]`, see `NavPolicy::pinned_origin`) only a page on that origin may;
/// a request from any other page, or from an unknown one, is refused so a foreign page cannot spam
/// or phrase the dialog. Without a pin the shell cannot tell the app's pages from others and the
/// page counts as the app's own.
pub fn consent_surface(pinned: Option<&str>, current: Option<&Url>) -> ConsentSurface {
    match pinned {
        None => ConsentSurface::Prompt,
        Some(pin) => match current.and_then(origin_of) {
            Some(origin) if origin == pin => ConsentSurface::Prompt,
            _ => ConsentSurface::Refuse,
        },
    }
}

/// An app's consent setup: where grants live and the words for each scheme.
pub struct ProtocolConsent {
    pub store: Box<dyn GrantStore>,
    /// The prompt for one scheme (lowercase, without the colon).
    pub prompt: Box<dyn Fn(&str) -> ConsentPrompt + Send + Sync>,
}

impl ProtocolConsent {
    pub fn new(
        store: impl GrantStore,
        prompt: impl Fn(&str) -> ConsentPrompt + Send + Sync + 'static,
    ) -> Self {
        Self {
            store: Box::new(store),
            prompt: Box::new(prompt),
        }
    }

    /// Whether `scheme` may be handed to the system now: a remembered grant answers at once, else
    /// the dialog asks. "Always allow" is stored before this returns `true`; a failure to store it
    /// still allows this one open and is returned as the error's absence of a grant (the next
    /// request asks again). Any dialog failure is `false`.
    pub fn confirm(&self, scheme: &str) -> bool {
        let scheme = scheme.to_ascii_lowercase();
        if self.store.is_granted(&scheme).unwrap_or(false) {
            return true;
        }
        let prompt = (self.prompt)(&scheme);
        match ask(&prompt) {
            Ok(ConsentChoice::AllowOnce) => true,
            Ok(ConsentChoice::AlwaysAllow) => {
                let _ = self.store.grant(&scheme);
                true
            }
            Ok(ConsentChoice::Deny) | Err(_) => false,
        }
    }
}

/// Show the native dialog and block until the person answers. Never call it on the main thread
/// while the event loop must keep running; the shell calls it from a worker thread.
pub fn ask(prompt: &ConsentPrompt) -> Result<ConsentChoice, ShellError> {
    prompt.validate()?;
    #[cfg(windows)]
    return ask_windows(prompt);
    #[cfg(target_os = "macos")]
    return ask_macos(prompt);
    #[cfg(not(any(windows, target_os = "macos")))]
    Err(ShellError::Unsupported(
        "native consent dialog is not available on this platform".into(),
    ))
}

#[cfg(windows)]
fn ask_windows(prompt: &ConsentPrompt) -> Result<ConsentChoice, ShellError> {
    use crate::winapi::windows::core::PCWSTR;
    use crate::winapi::windows::Win32::Foundation::HWND;
    use crate::winapi::windows::Win32::UI::Controls::{
        TaskDialogIndirect, TASKDIALOGCONFIG, TASKDIALOG_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION,
    };

    const ID_ALLOW_ONCE: i32 = 1001;
    const ID_ALWAYS: i32 = 1002;
    const ID_DENY: i32 = 1003;

    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let title = wide(&prompt.title);
    let heading = wide(&prompt.heading);
    let detail = wide(&prompt.detail);
    let allow_once = wide(&prompt.allow_once_label);
    let always = wide(&prompt.always_allow_label);
    let deny = wide(&prompt.deny_label);

    // Left to right: the safest first reading order is Allow once, Always allow, Deny; Deny is the
    // default and the answer to Esc and the close box.
    let buttons = [
        TASKDIALOG_BUTTON {
            nButtonID: ID_ALLOW_ONCE,
            pszButtonText: PCWSTR(allow_once.as_ptr()),
        },
        TASKDIALOG_BUTTON {
            nButtonID: ID_ALWAYS,
            pszButtonText: PCWSTR(always.as_ptr()),
        },
        TASKDIALOG_BUTTON {
            nButtonID: ID_DENY,
            pszButtonText: PCWSTR(deny.as_ptr()),
        },
    ];

    let mut config = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: HWND(prompt.parent_window.unwrap_or(0) as *mut std::ffi::c_void),
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        pszMainInstruction: PCWSTR(heading.as_ptr()),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: ID_DENY,
        ..Default::default()
    };
    if !prompt.detail.is_empty() {
        config.pszContent = PCWSTR(detail.as_ptr());
    }

    let mut pressed: i32 = 0;
    // SAFETY: `config` is fully initialised for this call (`cbSize` set; every pointer it holds is a
    // NUL-terminated UTF-16 buffer or the `buttons` array, all alive until the call returns). The
    // dialog is modal and returns the pressed button id through `pressed`. `TaskDialogIndirect`
    // needs Common Controls v6, which every Tauri app (and this crate's test and bench binaries,
    // via build.rs) carries in its manifest.
    let shown = unsafe { TaskDialogIndirect(&config, Some(&mut pressed as *mut i32), None, None) };
    shown.map_err(|e| ShellError::Other(format!("consent dialog: {e}")))?;
    Ok(match pressed {
        ID_ALLOW_ONCE => ConsentChoice::AllowOnce,
        ID_ALWAYS => ConsentChoice::AlwaysAllow,
        // ID_DENY, IDCANCEL (Esc, the close box) and anything unexpected.
        _ => ConsentChoice::Deny,
    })
}

#[cfg(target_os = "macos")]
fn ask_macos(prompt: &ConsentPrompt) -> Result<ConsentChoice, ShellError> {
    // The script is fixed; the strings travel as argv. Deny is the default and the cancel button.
    let output = std::process::Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "set answer to display alert (item 1 of argv) message (item 2 of argv) buttons {item 3 of argv, item 4 of argv, item 5 of argv} default button (item 3 of argv) cancel button (item 3 of argv)",
            "-e",
            "return button returned of answer",
            "-e",
            "end run",
            "--",
            &prompt.heading,
            &prompt.detail,
            &prompt.deny_label,
            &prompt.allow_once_label,
            &prompt.always_allow_label,
        ])
        .output()
        .map_err(|e| ShellError::Other(format!("consent dialog: {e}")))?;
    if !output.status.success() {
        // The cancel button (Deny, Esc) raises "User canceled" (-128).
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("-128") {
            return Ok(ConsentChoice::Deny);
        }
        return Err(ShellError::Other(format!(
            "consent dialog: osascript exited with {}: {}",
            output.status,
            stderr.trim()
        )));
    }
    let answer = String::from_utf8_lossy(&output.stdout);
    let answer = answer.trim_end_matches(['\r', '\n']);
    Ok(if answer == prompt.allow_once_label {
        ConsentChoice::AllowOnce
    } else if answer == prompt.always_allow_label {
        ConsentChoice::AlwaysAllow
    } else {
        ConsentChoice::Deny
    })
}
