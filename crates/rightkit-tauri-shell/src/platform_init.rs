//! The native platform name, injected before any page script runs.
//!
//! The renderer reads `window.__RIGHTKIT_PLATFORM__` synchronously until the OS plugin answers, so a
//! build that sets it first never reads a wrong platform (an iPad whose user agent says Macintosh
//! would otherwise read as macOS). A plugin init script applies to every webview, including
//! config-declared and runtime-built windows.
//!
//! The desktop shell plugin already injects this script in every profile. A builder that does not
//! install the shell (a mobile build) registers [`plugin`] instead; this module has no
//! dependency on any shell state, so it compiles on every target.

/// The platform name the renderer accepts, from the compile target: `macos`, `ios`, `windows` or
/// `linux`. `None` on any other target (Android), so the renderer keeps its own fallback.
pub fn native_platform_name() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("macos")
    } else if cfg!(target_os = "ios") {
        Some("ios")
    } else if cfg!(target_os = "windows") {
        Some("windows")
    } else if cfg!(target_os = "linux") {
        Some("linux")
    } else {
        None
    }
}

/// `window.__RIGHTKIT_PLATFORM__ = "<name>";` for the compile target, or `None` where
/// [`native_platform_name`] is `None`.
pub fn script() -> Option<String> {
    native_platform_name().map(|name| format!("window.__RIGHTKIT_PLATFORM__ = \"{name}\";"))
}

/// An init-script plugin that sets the platform global in every webview. The host passes the
/// plugin `name` (it must be unique among the app's plugins). On a target with no platform name
/// the plugin installs an empty script.
pub fn plugin<R: tauri::Runtime>(name: &'static str) -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::<R, ()>::new(name)
        .js_init_script(script().unwrap_or_default())
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_names_the_compile_target() {
        let script = script();
        assert_eq!(script.is_some(), native_platform_name().is_some());
        if let (Some(script), Some(name)) = (script, native_platform_name()) {
            assert_eq!(
                script,
                format!("window.__RIGHTKIT_PLATFORM__ = \"{name}\";")
            );
        }
    }
}
