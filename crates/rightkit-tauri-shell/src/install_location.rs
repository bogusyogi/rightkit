//! Install-location rules and launch-at-login wiring. Moved from host
//! `commands/build_location.rs` and `commands/updates.rs` (`installed_executable`,
//! `launch_at_login_spec`, `migrate_launch_at_login_entry`, and the bodies of the
//! `set_launch_at_login` / `launch_at_login_enabled` commands).
//!
//! Which executable paths are build or QA locations, never installed ones; the registry Run key
//! (Windows) and LaunchAgent (macOS) hold the login state, so nothing is stored by the app. The
//! host supplies the build-location folder names, the login launch argument, its wording and a
//! sink; the Tauri commands stay in the host and call these.

use std::path::{Path, PathBuf};

/// Host event sink: `(event, detail)`.
pub type EventSink = fn(&str, &str);

/// A path segment that marks a build or QA location. A Cargo `target` folder counts only in its
/// build layout (see [`is_build_location`]); a user or folder that happens to be named `target` is
/// an ordinary install location.
pub type BuildSegments<'a> = &'a [&'a str];

/// True when `path` is inside a build or QA location. Split on both separators so the answer
/// does not depend on the host that evaluates it.
pub fn is_build_location(path: &Path, build_segments: BuildSegments<'_>) -> bool {
    let text = path.to_string_lossy();
    let segments: Vec<&str> = text
        .split(['/', '\\'])
        .filter(|segment| !segment.is_empty())
        .collect();
    let is_profile = |segment: Option<&&str>| {
        segment.is_some_and(|name| {
            name.eq_ignore_ascii_case("debug") || name.eq_ignore_ascii_case("release")
        })
    };
    segments.iter().enumerate().any(|(index, segment)| {
        if build_segments
            .iter()
            .any(|known| segment.eq_ignore_ascii_case(known))
        {
            return true;
        }
        if !segment.eq_ignore_ascii_case("target") {
            return false;
        }
        // `target/<profile>/..` or `target/<target-triple>/<profile>/..`.
        is_profile(segments.get(index + 1))
            || (segments
                .get(index + 1)
                .is_some_and(|next| next.contains('-'))
                && is_profile(segments.get(index + 2)))
    })
}

/// The macOS rule: an installed app lives in `/Applications` or `~/Applications`. A mounted DMG
/// (`/Volumes/..`), an App Translocation copy (`.../AppTranslocation/..`) and any scratch or
/// re-signed copy elsewhere are not installed, and the LaunchAgent plist stores the executable
/// path, so rewriting it from one of them strands the login launch on a path that disappears.
pub fn is_macos_installed_location(path: &Path, home: Option<&Path>) -> bool {
    let text = path.to_string_lossy();
    if text.split('/').any(|segment| segment == "AppTranslocation") {
        return false;
    }
    let under = |root: &Path| {
        path.strip_prefix(root).is_ok_and(|rest| {
            // `<root>/<anything>.app/..`, so a stray binary dropped in the folder does not count.
            rest.components().any(|component| {
                component
                    .as_os_str()
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .ends_with(".app")
            })
        })
    };
    under(Path::new("/Applications")) || home.is_some_and(|home| under(&home.join("Applications")))
}

/// Eligibility for registering or repairing a login entry: macOS requires an installed location
/// ([`is_macos_installed_location`]); other platforms accept any path that is not a build location.
pub fn is_eligible_install_location(path: &Path, build_segments: BuildSegments<'_>) -> bool {
    if is_build_location(path, build_segments) {
        return false;
    }
    if cfg!(target_os = "macos") {
        let home = std::env::home_dir();
        return is_macos_installed_location(path, home.as_deref());
    }
    true
}

/// The host's launch-at-login values.
#[derive(Clone, Copy)]
pub struct LoginLaunch<'a> {
    /// Build or QA folder names (see [`is_build_location`]).
    pub build_segments: BuildSegments<'a>,
    /// Argument the login entry carries, so a login launch can be told from a manual one
    /// (`rightkit_tauri_shell::launch_origin` reads it).
    pub login_arg: &'a str,
    /// Error returned when the app does not run from an installed location (the host's wording).
    pub not_installed_message: &'a str,
    pub sink: EventSink,
}

/// This executable's path when it runs from an installed location; `None` from a build or QA
/// location (and on macOS from anywhere outside /Applications or ~/Applications, such as a mounted
/// DMG or an App Translocation copy), so nothing there is ever registered or repaired for login.
pub fn installed_executable(login: &LoginLaunch<'_>) -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    if !is_eligible_install_location(&executable, login.build_segments) {
        (login.sink)(
            "launch_at_login_skipped",
            "not running from an installed location (build or QA folder, mounted disk image, translocated or scratch copy); login launch is set from the installed app",
        );
        return None;
    }
    Some(executable)
}

/// The autostart entry for this app, or `None` outside an installed location. The login launch
/// yields the disk during the login storm (`low_priority_io`: on 2026-07-27 a cold boot took 52 s
/// to first UI while `syspolicyd` burned 90 s of CPU).
pub fn launch_at_login_spec(
    app: &tauri::AppHandle,
    login: &LoginLaunch<'_>,
) -> Option<crate::autostart::AutostartSpec> {
    use crate::autostart::{AutostartOptions, AutostartSpec};
    let executable = installed_executable(login)?;
    let legacy = app.package_info().name.clone();
    Some(AutostartSpec {
        app_id: app.config().identifier.clone(),
        app_name: legacy.clone(),
        executable,
        options: AutostartOptions {
            args: vec![login.login_arg.to_owned()],
            low_priority_io: true,
            migrate_from_plugin_entry: true,
            ..AutostartOptions::default()
        },
        legacy_plugin_name: Some(legacy),
    })
}

/// One-time carry-over of the entry `tauri-plugin-autostart` wrote in earlier builds, so an
/// upgraded user keeps their login-at-start choice and never has two login items. An enabled entry
/// is rewritten with this installed path, so one left behind by an instance that ran elsewhere (a
/// build folder, an older install location) points here again. Both platforms: the Windows Run key
/// and the macOS LaunchAgent plist each store the executable path.
pub fn migrate_launch_at_login_entry(app: &tauri::AppHandle, login: &LoginLaunch<'_>) {
    let Some(spec) = launch_at_login_spec(app, login) else {
        return;
    };
    let legacy = spec.legacy_plugin_name.clone().unwrap_or_default();
    match crate::autostart::migrate_plugin_entry(&spec, &legacy) {
        Ok(_) => {}
        Err(err) => (login.sink)("launch_at_login_migrate_failed", &err),
    }
    if crate::autostart::status(&spec).enabled {
        if let Err(err) = crate::autostart::set_enabled(&spec, true) {
            (login.sink)("launch_at_login_repair_failed", &err);
        }
    }
}

/// Enable or disable launch at login. Blocking registry/plist IO: call off the main thread.
pub fn set_launch_at_login(
    app: &tauri::AppHandle,
    login: &LoginLaunch<'_>,
    enabled: bool,
) -> Result<(), String> {
    let spec = launch_at_login_spec(app, login).ok_or(login.not_installed_message)?;
    crate::autostart::set_enabled(&spec, enabled)?;
    (login.sink)(
        "launch_at_login_set",
        if enabled {
            "enabled=true"
        } else {
            "enabled=false"
        },
    );
    Ok(())
}

/// Whether the login entry is enabled. Blocking: call off the main thread.
pub fn launch_at_login_enabled(
    app: &tauri::AppHandle,
    login: &LoginLaunch<'_>,
) -> Result<bool, String> {
    let spec = launch_at_login_spec(app, login).ok_or(login.not_installed_message)?;
    Ok(crate::autostart::status(&spec).enabled)
}
