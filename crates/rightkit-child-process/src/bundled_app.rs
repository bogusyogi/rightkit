//! Locating a nested `.app` directory inside the running app's bundle (macOS), and a
//! detection-only code-signature probe.
//!
//! [`resolve_bundled_app`] resolves a bundle-relative path such as
//! `Contents/Library/LoginItems/<Name>.app` to the canonical directory, refusing a path that
//! escapes the canonical bundle. It is the directory counterpart of
//! [`resolve_bundled_login_item`](crate::resolve_bundled_login_item), which resolves the
//! executable inside a nested app. [`probe_codesign`] runs `codesign -v` and reports the outcome
//! to a host event sink; it waits with a deadline & never enforces (use
//! [`verify_team_signature`](crate::verify_team_signature) to enforce).

use crate::bundled_helper::{bundle_root, missing};
use crate::BundledHelperError;
use std::path::{Component, Path, PathBuf};

/// Resolve `relative` (for example `Contents/Library/LoginItems/Helper.app`) against the `.app`
/// bundle that contains the running executable. `relative` must be a relative path made only of
/// normal components ending in `.app`. The result is canonical, has a `Contents` directory,
/// & lies strictly under the canonical bundle root. This verifies placement only, never signer
/// or bundle identity, & never launches anything. `Ok(None)` means there is nothing to resolve
/// (the executable is not inside an `.app`, or the app is absent: an unpackaged run);
/// an app that exists but escapes the bundle or is
/// not a directory is an error ([`BundledHelperError::NotFound`]), and a bad `relative` is
/// [`BundledHelperError::InvalidName`].
pub fn resolve_bundled_app(relative: &str) -> Result<Option<PathBuf>, BundledHelperError> {
    let exe = std::env::current_exe().map_err(|e| {
        BundledHelperError::NotFound(format!(
            "cannot locate the running executable: {}",
            e.kind()
        ))
    })?;
    resolve_app_in_bundle(&exe, relative)
}

/// The path logic of [`resolve_bundled_app`], independent of the running executable (`exe` is any
/// file inside the main `.app`).
pub fn resolve_app_in_bundle(
    exe: &Path,
    relative: &str,
) -> Result<Option<PathBuf>, BundledHelperError> {
    let rel = Path::new(relative);
    let normal_only = !relative.is_empty()
        && rel
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if !normal_only || relative.contains('\\') || relative.chars().any(char::is_control) {
        return Err(BundledHelperError::InvalidName(
            "must be a relative path of normal components".to_string(),
        ));
    }
    if !rel
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
    {
        return Err(BundledHelperError::InvalidName(
            "must name a nested .app bundle".to_string(),
        ));
    }
    let Ok(bundle) = bundle_root(exe) else {
        return Ok(None);
    };
    let app = match bundle.join(rel).canonicalize() {
        Ok(app) => app,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(missing(&format!(
                "app cannot be resolved: {}",
                error.kind()
            )))
        }
    };
    if app == bundle || !app.starts_with(&bundle) {
        return Err(missing("app resolves outside the app bundle"));
    }
    if !app.is_dir() {
        return Err(missing("app is not a directory"));
    }
    if !app
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
        || !app.join("Contents").is_dir()
    {
        return Err(missing("app is not an .app bundle directory"));
    }
    Ok(Some(app))
}

/// Best-effort `codesign -v` over `path` (macOS; a no-op elsewhere). Detection only: a failure or
/// a probe that could not run is reported to `event_sink` as
/// `("helper_codesign_invalid", "path=.. status=..")` or
/// `("helper_codesign_probe_failed", "path=.. err=..")`. Waits up to twenty seconds before
/// bounded tree termination & reaping; call off the main thread. Neither signature validity
/// nor bundle placement proves expected signer or bundle identity. Use
/// [`crate::verify_team_signature`] separately when an expected team must be enforced.
#[cfg(target_os = "macos")]
pub fn probe_codesign(path: &Path, event_sink: fn(&str, &str)) {
    use std::process::Stdio;
    let mut owned = crate::OwnedCommand::new("/usr/bin/codesign");
    owned
        .command_mut()
        .arg("-v")
        .arg("--")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match crate::status_with_timeout(owned, std::time::Duration::from_secs(20)) {
        Ok(Some(status)) if status.success() => {}
        Ok(Some(status)) => event_sink(
            "helper_codesign_invalid",
            &format!("path={} status={status}", path.display()),
        ),
        Ok(None) => event_sink(
            "helper_codesign_probe_failed",
            &format!("path={} err=timeout", path.display()),
        ),
        Err(err) => event_sink(
            "helper_codesign_probe_failed",
            &format!("path={} err={err}", path.display()),
        ),
    }
}

/// No-op off macOS (there is no `codesign`).
#[cfg(not(target_os = "macos"))]
pub fn probe_codesign(_path: &Path, _event_sink: fn(&str, &str)) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "rightkit-bundled-app-{}-{nonce}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }

        fn executable(&self) -> PathBuf {
            let exe = self.0.join("Host.app/Contents/MacOS/host");
            std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
            std::fs::write(&exe, b"host").unwrap();
            exe
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn rejects_invalid_relative_paths_before_bundle_lookup() {
        for relative in [
            "",
            "/Helper.app",
            "../Helper.app",
            "Contents/../Helper.app",
            "Contents\\Helper.app",
            "Contents/Helper",
            "Contents/Helper.app\n",
        ] {
            assert!(matches!(
                resolve_app_in_bundle(Path::new("absent"), relative),
                Err(BundledHelperError::InvalidName(_)),
            ));
        }
    }

    #[test]
    fn resolves_only_existing_nested_app_directories() {
        let fixture = Fixture::new();
        let exe = fixture.executable();
        let relative = "Contents/Library/LoginItems/Helper.app";
        assert_eq!(resolve_app_in_bundle(&exe, relative).unwrap(), None);
        let app = fixture.0.join("Host.app").join(relative);
        std::fs::create_dir_all(app.parent().unwrap()).unwrap();
        std::fs::write(&app, b"not a directory").unwrap();
        assert!(resolve_app_in_bundle(&exe, relative).is_err());
        std::fs::remove_file(&app).unwrap();
        std::fs::create_dir(&app).unwrap();
        assert!(resolve_app_in_bundle(&exe, relative).is_err());
        std::fs::create_dir(app.join("Contents")).unwrap();
        assert_eq!(
            resolve_app_in_bundle(&exe, relative).unwrap(),
            Some(app.canonicalize().unwrap()),
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_escape_and_alias_to_main_bundle() {
        let fixture = Fixture::new();
        let exe = fixture.executable();
        let outside = fixture.0.join("Outside.app");
        std::fs::create_dir_all(outside.join("Contents")).unwrap();
        let alias = fixture.0.join("Host.app/Contents/Helper.app");
        std::os::unix::fs::symlink(&outside, &alias).unwrap();
        assert!(resolve_app_in_bundle(&exe, "Contents/Helper.app").is_err());
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(fixture.0.join("Host.app"), &alias).unwrap();
        assert!(resolve_app_in_bundle(&exe, "Contents/Helper.app").is_err());
    }
}
