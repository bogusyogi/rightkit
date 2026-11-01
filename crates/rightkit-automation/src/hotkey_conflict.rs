//! Description of a global-shortcut registration failure caused by another app.

/// The shortcuts a dictation shell registers by default.
pub const REQUIRED_FIXED_SHORTCUTS: [&str; 3] = ["Ctrl+Space", "Ctrl+Shift+Space", "Ctrl+,"];

/// A registration failure and the shortcuts it concerns.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct HotkeyConflict {
    /// The shortcuts that failed to register.
    pub shortcuts: Vec<String>,
    /// User-facing explanation.
    pub message: String,
}

impl HotkeyConflict {
    /// Parse a `register <shortcut> failed: ...` error against [`REQUIRED_FIXED_SHORTCUTS`].
    pub fn from_registration_error(error: &str) -> Self {
        Self::from_registration_error_for(error, &REQUIRED_FIXED_SHORTCUTS)
    }

    /// Parse a `register <shortcut> failed: ...` error against the shortcuts the app registers.
    /// An error that names none of them reports all of them.
    pub fn from_registration_error_for(error: &str, required: &[&str]) -> Self {
        let shortcuts = error
            .strip_prefix("register ")
            .and_then(|rest| rest.split_once(" failed:").map(|(shortcut, _)| shortcut))
            .filter(|shortcut| required.contains(shortcut))
            .map(|shortcut| vec![shortcut.to_string()])
            .unwrap_or_else(|| {
                required
                    .iter()
                    .map(|shortcut| (*shortcut).to_string())
                    .collect()
            });
        Self {
            shortcuts,
            message: "Registration failed because another app is using one or more fixed shortcuts; change or close that app, then retry.".to_string(),
        }
    }
}
