//! Local files for demo mode: the on/off flag, the bundle folder and the content-free audit log.
//!
//! Everything lives under one folder, `<app data>/demo`. Nothing here is uploaded: this crate has no
//! uploader and no command in this module takes a URL.

use std::fs;
use std::path::{Path, PathBuf};

use rightkit_daemon::{AppRoot, AppRootError};
use rightkit_fs::{atomic_write, contain, contain_lenient};
use rightkit_logs::JsonlStore;
use serde_json::{json, Value};

pub struct Store {
    dir: PathBuf,
    audit: JsonlStore,
}

/// A bundle folder name: letters, digits and `_.:-`, starting with a letter or digit.
pub fn valid_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 128
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'-'))
}

impl Store {
    /// Startup refuses any path failure; this never resolves a second root.
    pub fn open(app_root: &AppRoot) -> Result<Store, AppRootError> {
        let dir = app_root.data_dir("demo")?;
        app_root.data_dir("demo/bundles")?;
        Ok(Store {
            audit: JsonlStore::new(dir.join("audit.jsonl"), 0),
            dir,
        })
    }

    pub fn bundles_root(&self) -> PathBuf {
        self.dir.join("bundles")
    }

    pub fn bundle_dir(&self, name: &str) -> Option<PathBuf> {
        valid_name(name)
            .then(|| contain_lenient(&self.bundles_root(), name).ok())
            .flatten()
    }

    fn flag(&self) -> PathBuf {
        self.dir.join("enabled")
    }

    /// Off unless the flag file says `1`. Any read problem reads as off.
    pub fn enabled(&self) -> bool {
        fs::read_to_string(self.flag())
            .map(|s| s.trim() == "1")
            .unwrap_or(false)
    }

    pub fn set_enabled(&self, on: bool) -> std::io::Result<()> {
        if on {
            atomic_write(&self.flag(), b"1").map_err(Into::into)
        } else {
            match fs::remove_file(self.flag()) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        }
    }

    /// One JSON line: what happened, never what was typed or shown. Failures are ignored on purpose;
    /// the audit log never blocks a kill.
    pub fn audit(&self, event: &str, fields: Value) {
        let mut line = json!({"ts_ms": now_ms(), "event": event});
        if let (Some(l), Some(f)) = (line.as_object_mut(), fields.as_object()) {
            for (k, v) in f {
                l.insert(k.clone(), v.clone());
            }
        }
        let _ = self.audit.append_value(&line);
    }

    /// Deletes one sealed bundle folder. Sealed bundles are read-only, so write access is restored
    /// first. Refuses a name that is not a plain folder name and a folder that resolves outside the
    /// bundles root.
    pub fn remove_bundle(&self, name: &str) -> Result<(), String> {
        if !valid_name(name) {
            return Err("that is not a bundle name".into());
        }
        let real = contain(&self.bundles_root(), name).map_err(|e| e.to_string())?;
        make_writable(&real);
        fs::remove_dir_all(&real).map_err(|e| e.to_string())
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// The std call is the only portable way to clear the read-only flag; the Unix hazard it warns of
// (world-writable) does not apply because the owner bits are set explicitly first.
#[allow(clippy::permissions_set_readonly_false)]
fn make_writable(path: &Path) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    if meta.file_type().is_symlink() {
        return;
    }
    let mut permissions = meta.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(permissions.mode() | 0o700);
    }
    #[cfg(not(unix))]
    {
        permissions.set_readonly(false);
    }
    let _ = fs::set_permissions(path, permissions);
    if meta.is_dir() {
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                make_writable(&entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Unit test on purpose: the name check is the guard in front of a recursive delete, and no
    // end-to-end run should try a hostile name against a real folder.
    #[test]
    fn bundle_names_cannot_escape_the_bundles_folder() {
        assert!(valid_name("demo-0a1b2c3d4e5f"));
        for bad in ["", "..", "../x", "a/b", "a\\b", ".hidden", "a b", "/etc"] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn discard_sealed_read_only_bundle_stays_inside_store() {
        let temp = tempfile::tempdir().unwrap();
        let app_root = rightkit_daemon::Suite::at(temp.path().join("shared"))
            .unwrap()
            .app_root("TestApp")
            .unwrap();
        let store = Store::open(&app_root).unwrap();
        let bundle = store.bundle_dir("demo-sealed").unwrap();
        fs::create_dir_all(bundle.join("media")).unwrap();
        let recording = bundle.join("media/recording.mp4");
        fs::write(&recording, b"sealed recording").unwrap();
        for path in [&recording, &bundle.join("media"), &bundle] {
            let mut permissions = fs::metadata(path).unwrap().permissions();
            permissions.set_readonly(true);
            fs::set_permissions(path, permissions).unwrap();
        }
        let outside = temp.path().join("keep");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("original"), b"keep me").unwrap();
        assert!(store.remove_bundle("../keep").is_err());
        assert!(store.remove_bundle(outside.to_str().unwrap()).is_err());
        store.remove_bundle("demo-sealed").unwrap();
        assert!(!bundle.exists());
        assert_eq!(fs::read(outside.join("original")).unwrap(), b"keep me");
        assert!(store.bundles_root().is_dir());
    }

    // Hostile symlinks exercise the recursive-delete guard without exposing a real user tree.
    #[cfg(any(unix, windows))]
    #[test]
    fn discard_refuses_bundle_symlink_outside_store() {
        let temp = tempfile::tempdir().unwrap();
        let app_root = rightkit_daemon::Suite::at(temp.path().join("shared"))
            .unwrap()
            .app_root("TestApp")
            .unwrap();
        let store = Store::open(&app_root).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("original"), b"keep me").unwrap();
        let link = store.bundles_root().join("demo-escape");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&outside, &link)
            .expect("discard security test requires symlink permission");
        assert!(store.remove_bundle("demo-escape").is_err());
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(outside.join("original")).unwrap(), b"keep me");
    }
}
