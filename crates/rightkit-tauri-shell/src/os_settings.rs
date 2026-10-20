//! Open a pane of the operating system's Settings app by a typed [`SettingsPane`].
//!
//! Donors: ViewRight `commands/mac_associations/windows.rs` `open_default_apps`
//! (`explorer.exe ms-settings:defaultapps`) and HeardRight's hand-written `open
//! x-apple.systempreferences:...` calls for the microphone, accessibility and files panes
//! (`commands/microphone.rs`, `system.rs`, `files.rs`). The pane is a closed enum and each pane maps
//! to a fixed URI per OS, so no renderer text becomes part of a launch command; the one open-ended
//! form, the Windows per-app Default apps page, takes an app name that is validated and
//! percent-encoded ([`default_apps_uri_for`]).
//!
//! * macOS: `/usr/bin/open x-apple.systempreferences:com.apple.preference.security?Privacy_<Pane>`.
//! * Windows: `explorer.exe ms-settings:<page>`, one argv element, never through a shell.
//! * Elsewhere: [`ShellError::Unsupported`].
//!
//! This is a different launcher than [`crate::opener`], which deliberately refuses every scheme except
//! http(s) and mailto.

use serde::{Deserialize, Serialize};

use crate::opener::{ExternalOpener, SystemOpener};
use crate::ShellError;

/// Windows Settings, Default apps.
pub const DEFAULT_APPS: &str = "ms-settings:defaultapps";

const MS_SETTINGS: &str = "ms-settings:";
const MAC_SECURITY: &str = "x-apple.systempreferences:com.apple.preference.security";

/// A Settings pane the shell can open. The wire form is kebab-case (`"default-apps"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SettingsPane {
    /// Privacy: microphone access.
    Microphone,
    /// Privacy: camera access.
    Camera,
    /// macOS Privacy & Security > Accessibility; Windows Accessibility (Ease of Access).
    Accessibility,
    /// macOS Privacy & Security > Files and Folders; Windows Privacy > File system.
    Files,
    /// Windows Default apps. macOS has no default-apps pane: it opens System Settings > General,
    /// where the system's default web browser choice lives on current releases.
    DefaultApps,
}

/// The host operating system, as a value so the mapping is testable on every host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Macos,
    Windows,
    Other,
}

impl Os {
    /// The OS this build runs on.
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

/// The URI that opens `pane` on `os`, or `None` where the OS has no such pane.
pub fn settings_uri(pane: SettingsPane, os: Os) -> Option<String> {
    match os {
        Os::Macos => Some(match pane {
            SettingsPane::Microphone => format!("{MAC_SECURITY}?Privacy_Microphone"),
            SettingsPane::Camera => format!("{MAC_SECURITY}?Privacy_Camera"),
            SettingsPane::Accessibility => format!("{MAC_SECURITY}?Privacy_Accessibility"),
            SettingsPane::Files => format!("{MAC_SECURITY}?Privacy_FilesAndFolders"),
            SettingsPane::DefaultApps => {
                "x-apple.systempreferences:com.apple.preference.general".to_owned()
            }
        }),
        Os::Windows => Some(
            match pane {
                SettingsPane::Microphone => "ms-settings:privacy-microphone",
                SettingsPane::Camera => "ms-settings:privacy-webcam",
                SettingsPane::Accessibility => "ms-settings:easeofaccess",
                SettingsPane::Files => "ms-settings:privacy-broadfilesystemaccess",
                SettingsPane::DefaultApps => DEFAULT_APPS,
            }
            .to_owned(),
        ),
        Os::Other => None,
    }
}

/// `Ok(uri)` for `ms-settings:<page>[:<subpage>...]` with ASCII letters, digits, `-`, `_`, `.` and
/// `:` only (case-insensitive scheme), optionally followed by the single query the per-app Default
/// apps page takes: `?registeredAppUser=<percent-encoded name>`. Anything else is refused.
pub fn validate_settings_uri(uri: &str) -> Result<&str, ShellError> {
    let uri = uri.trim();
    let refused = || ShellError::Invalid(format!("{uri:?} is not a plain ms-settings: page"));
    let rest = uri
        .get(..MS_SETTINGS.len())
        .filter(|scheme| scheme.eq_ignore_ascii_case(MS_SETTINGS))
        .map(|_| &uri[MS_SETTINGS.len()..])
        .ok_or_else(|| ShellError::Invalid(format!("{uri:?} is not an ms-settings: URI")))?;
    let (page, query) = match rest.split_once('?') {
        Some((page, query)) => (page, Some(query)),
        None => (rest, None),
    };
    let page_ok = !page.is_empty()
        && !page.starts_with(':')
        && !page.ends_with(':')
        && !page.contains("::")
        && page
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'));
    let query_ok = query.is_none_or(|q| {
        q.strip_prefix("registeredAppUser=").is_some_and(|name| {
            !name.is_empty()
                && name.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'%' | b'-' | b'_' | b'.' | b'~')
                })
        })
    });
    if page_ok && query_ok {
        Ok(uri)
    } else {
        Err(refused())
    }
}

/// The Windows Default apps page for one registered app:
/// `ms-settings:defaultapps?registeredAppUser=<name>`. `registered_app_name` is the name the app
/// registered under `HKCU\Software\RegisteredApplications`; letters, digits, spaces and `. _ - ( )`
/// up to 64 characters are accepted and percent-encoded.
pub fn default_apps_uri_for(registered_app_name: &str) -> Result<String, ShellError> {
    let name = registered_app_name.trim();
    let ok = !name.is_empty()
        && name.chars().count() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-' | '(' | ')'));
    if !ok {
        return Err(ShellError::Invalid(format!(
            "{registered_app_name:?} is not a usable registered app name"
        )));
    }
    let mut encoded = String::with_capacity(name.len() * 3);
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-') {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    Ok(format!("{DEFAULT_APPS}?registeredAppUser={encoded}"))
}

/// Open `pane` for `os` through `opener` (the seam tests and other launchers use).
pub fn open_pane_with(
    opener: &dyn ExternalOpener,
    pane: SettingsPane,
    os: Os,
) -> Result<(), ShellError> {
    let uri = settings_uri(pane, os).ok_or_else(|| {
        ShellError::Unsupported("Settings panes open on macOS and Windows only".into())
    })?;
    opener.open(&uri).map_err(ShellError::Other)
}

/// Open `pane` on this OS. `ShellError::Unsupported` elsewhere than macOS and Windows.
pub fn open_pane(pane: SettingsPane) -> Result<(), ShellError> {
    match Os::current() {
        Os::Windows => {
            open_settings_uri(&settings_uri(pane, Os::Windows).expect("every pane maps on Windows"))
        }
        os => open_pane_with(&SystemOpener, pane, os),
    }
}

/// Open a Windows Settings page by validated `ms-settings:` URI with `explorer.exe`.
/// `ShellError::Unsupported` elsewhere.
pub fn open_settings_uri(uri: &str) -> Result<(), ShellError> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};

        let uri = validate_settings_uri(uri)?;
        let mut child = Command::new("explorer.exe");
        child
            .arg(uri)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // CREATE_NO_WINDOW: never flash a console.
            .creation_flags(0x0800_0000);
        child
            .spawn()
            .map(|mut child| {
                // explorer.exe's exit code is not a success signal; reap without blocking.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            })
            .map_err(|e| ShellError::Other(format!("could not open {uri}: {e}")))
    }
    #[cfg(not(windows))]
    {
        let _ = uri;
        Err(ShellError::Unsupported(
            "Settings URIs open on Windows only".into(),
        ))
    }
}

/// Validate `uri` and hand it to `opener` (the seam tests and other launchers use).
pub fn open_settings_uri_with(opener: &dyn ExternalOpener, uri: &str) -> Result<(), ShellError> {
    let uri = validate_settings_uri(uri)?;
    opener.open(uri).map_err(ShellError::Other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);

    impl ExternalOpener for Recorder {
        fn open(&self, target: &str) -> Result<(), String> {
            self.0.lock().unwrap().push(target.to_owned());
            Ok(())
        }
    }

    const ALL: [SettingsPane; 5] = [
        SettingsPane::Microphone,
        SettingsPane::Camera,
        SettingsPane::Accessibility,
        SettingsPane::Files,
        SettingsPane::DefaultApps,
    ];

    #[test]
    fn macos_privacy_panes_map_to_the_security_pane_anchors() {
        let uri = |p| settings_uri(p, Os::Macos).unwrap();
        assert_eq!(
            uri(SettingsPane::Microphone),
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
        );
        assert!(uri(SettingsPane::Camera).ends_with("?Privacy_Camera"));
        assert!(uri(SettingsPane::Accessibility).ends_with("?Privacy_Accessibility"));
        assert!(uri(SettingsPane::Files).ends_with("?Privacy_FilesAndFolders"));
        assert!(uri(SettingsPane::DefaultApps).starts_with("x-apple.systempreferences:"));
    }

    #[test]
    fn every_windows_pane_is_a_valid_plain_settings_uri() {
        for pane in ALL {
            let uri = settings_uri(pane, Os::Windows).unwrap();
            assert_eq!(validate_settings_uri(&uri).unwrap(), uri, "{pane:?}");
        }
        assert_eq!(
            settings_uri(SettingsPane::DefaultApps, Os::Windows).as_deref(),
            Some(DEFAULT_APPS)
        );
    }

    #[test]
    fn other_hosts_have_no_panes() {
        for pane in ALL {
            assert_eq!(settings_uri(pane, Os::Other), None);
        }
        let recorder = Recorder::default();
        assert!(matches!(
            open_pane_with(&recorder, SettingsPane::Camera, Os::Other),
            Err(ShellError::Unsupported(_))
        ));
        assert!(recorder.0.lock().unwrap().is_empty());
    }

    #[test]
    fn the_opener_receives_exactly_the_mapped_uri() {
        let recorder = Recorder::default();
        open_pane_with(&recorder, SettingsPane::Microphone, Os::Macos).unwrap();
        open_pane_with(&recorder, SettingsPane::Files, Os::Windows).unwrap();
        assert_eq!(
            *recorder.0.lock().unwrap(),
            [
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone",
                "ms-settings:privacy-broadfilesystemaccess"
            ]
        );
    }

    #[test]
    fn panes_use_kebab_case_on_the_wire() {
        assert_eq!(
            serde_json::to_string(&SettingsPane::DefaultApps).unwrap(),
            "\"default-apps\""
        );
        let p: SettingsPane = serde_json::from_str("\"microphone\"").unwrap();
        assert_eq!(p, SettingsPane::Microphone);
        assert!(serde_json::from_str::<SettingsPane>("\"ms-settings:x\"").is_err());
    }

    #[test]
    fn only_plain_settings_pages_validate() {
        for ok in [
            DEFAULT_APPS,
            "ms-settings:network-wifi",
            "MS-SETTINGS:privacy-microphone",
            "ms-settings:notifications:quiethours",
            "  ms-settings:defaultapps  ",
            "ms-settings:defaultapps?registeredAppUser=My%20App",
        ] {
            assert!(validate_settings_uri(ok).is_ok(), "{ok:?}");
        }
        for bad in [
            "",
            "ms-settings:",
            "ms-settings:?x=1",
            "ms-settings:defaultapps?x=1",
            "ms-settings:defaultapps?registeredAppUser=",
            "ms-settings:defaultapps?registeredAppUser=a&b=c",
            "ms-settings:defaultapps?registeredAppUser=a b",
            "ms-settings:a b",
            "ms-settings:a&calc",
            "ms-settings:a\"b",
            "ms-settings:a/../b",
            "ms-settings::a",
            "ms-settings:a:",
            "ms-settings:a::b",
            "ms-settings",
            "https://example.com",
            "file:///C:/Windows/System32/calc.exe",
            "calc.exe",
            "ms-settingsx:defaultapps",
            "ms-settin\u{e9}:a",
        ] {
            assert!(validate_settings_uri(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_per_app_page_encodes_a_validated_name() {
        assert_eq!(
            default_apps_uri_for("My App (Beta)").unwrap(),
            "ms-settings:defaultapps?registeredAppUser=My%20App%20%28Beta%29"
        );
        assert_eq!(
            default_apps_uri_for(" ViewRight ").unwrap(),
            "ms-settings:defaultapps?registeredAppUser=ViewRight"
        );
        let long = "x".repeat(65);
        for bad in [
            "",
            "   ",
            "a&b",
            "a=b",
            "a/b",
            "a\"b",
            "é",
            "a\nb",
            long.as_str(),
        ] {
            assert!(default_apps_uri_for(bad).is_err(), "{bad:?}");
        }
        let uri = default_apps_uri_for("My App (Beta)").unwrap();
        assert!(validate_settings_uri(&uri).is_ok());
    }

    #[test]
    fn the_settings_uri_opener_receives_only_validated_uris() {
        let recorder = Recorder::default();
        open_settings_uri_with(&recorder, " ms-settings:defaultapps ").unwrap();
        assert!(open_settings_uri_with(&recorder, "ms-settings:a&calc").is_err());
        assert!(open_settings_uri_with(&recorder, "https://example.com").is_err());
        assert_eq!(*recorder.0.lock().unwrap(), ["ms-settings:defaultapps"]);
    }

    #[cfg(not(windows))]
    #[test]
    fn explorer_launch_is_windows_only() {
        assert!(matches!(
            open_settings_uri(DEFAULT_APPS),
            Err(ShellError::Unsupported(_))
        ));
    }
}
