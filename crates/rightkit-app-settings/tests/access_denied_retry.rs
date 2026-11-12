//! The opt-in retry of Windows access denied (error 5) on the replace:
//! `StoreOptions::retry_access_denied` and `FileBackend::retry_access_denied`.
//!
//! The retry itself is `rightkit_fs::AccessDeniedRetry` and only Windows reports the error, so the
//! tests every platform runs prove the option changes nothing else about a write; the Windows
//! module holds the file open the way the retry exists for.

use rightkit_app_settings::{
    Edit, FileBackend, FileFormat, Schema, SettingsBackend, Store, StoreOptions, Stored,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Prefs {
    theme: String,
}

fn fresh_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create the test directory");
    dir
}

fn options() -> StoreOptions {
    StoreOptions::new(Schema::new("probe", 1)).format(FileFormat::Flat)
}

#[test]
fn a_store_with_the_retry_writes_and_reads_back() {
    let path = fresh_dir("access-denied-store").join("settings.json");
    let retrying = options().retry_access_denied(3, Duration::from_millis(5));
    let store = Store::<Prefs>::open_path(&path, retrying.clone()).expect("open");
    store
        .update(|prefs| {
            prefs.theme = "dark".into();
            Ok(())
        })
        .expect("first write");
    store
        .update(|prefs| {
            prefs.theme = "light".into();
            Ok(())
        })
        .expect("replace of an existing file");
    drop(store);
    let reopened = Store::<Prefs>::open_path(&path, retrying).expect("reopen");
    assert_eq!(reopened.get().theme, "light");
}

#[test]
fn a_hand_driven_backend_takes_the_same_retry() {
    let path = fresh_dir("access-denied-backend").join("settings.json");
    let backend =
        FileBackend::new(&path, FileFormat::Flat).retry_access_denied(3, Duration::from_millis(5));
    for theme in ["dark", "light"] {
        backend
            .patch(&mut |current| {
                let mut values = current.map(|c| c.values.clone()).unwrap_or_default();
                values.insert("theme".into(), theme.into());
                Ok(Edit::Commit(Stored {
                    version: None,
                    namespace: None,
                    values,
                    revision: 0,
                    needs_rewrite: false,
                }))
            })
            .expect("commit");
    }
    let stored = backend.load().expect("load").expect("a stored document");
    assert_eq!(stored.values["theme"], "light");
}

/// A reader that opened the settings file without delete sharing makes the default replace fail
/// with error 5; the opt-in waits for it to let go.
#[cfg(windows)]
mod windows {
    use super::*;
    use rightkit_app_settings::Error;
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const ERROR_ACCESS_DENIED: i32 = 5;

    fn hold_without_delete_sharing(path: &Path) -> fs::File {
        fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(path)
            .expect("open the settings file without delete sharing")
    }

    fn set_theme(store: &Store<Prefs>, theme: &str) -> Result<(), Error> {
        store.update(|prefs| {
            prefs.theme = theme.into();
            Ok(())
        })
    }

    #[test]
    fn the_retry_outlasts_a_reader_that_blocks_the_replace() {
        let path = fresh_dir("access-denied-held").join("settings.json");
        let plain = Store::<Prefs>::open_path(&path, options()).expect("open");
        set_theme(&plain, "dark").expect("write with nobody holding the file");

        let held = hold_without_delete_sharing(&path);
        match set_theme(&plain, "light") {
            Err(Error::Io(e)) => assert_eq!(e.raw_os_error(), Some(ERROR_ACCESS_DENIED)),
            other => panic!("the default replace must fail with error 5, got {other:?}"),
        }
        assert_eq!(plain.get().theme, "dark", "a failed write leaves the file");

        let retrying = Store::<Prefs>::open_path(
            &path,
            options().retry_access_denied(100, Duration::from_millis(20)),
        )
        .expect("open with the retry");
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            drop(held);
        });
        set_theme(&retrying, "light").expect("the write lands once the reader lets go");
        release.join().expect("the releasing thread");
        assert_eq!(retrying.get().theme, "light");
    }
}
