//! A step that keeps the previous content fails the operation when it fails: nothing is written
//! over a damaged file that could not be set aside, over a copy that could not be made, or over a
//! file whose `.bak` could not be refreshed.
//!
//! The failures here are real file-system ones (a directory sits where a file must go), so they
//! run on every platform. `src/backend.rs` has the injected-failure variants.
use rightkit_app_settings::{
    Error, OnCorrupt, PreserveStep, QuarantineName, Schema, Store, StoreOptions,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
struct Cfg {
    n: u32,
}

fn schema() -> Schema {
    Schema::new("keep", 1)
}

/// File names in `dir` other than the lock sidecar.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| !name.ends_with(".lock"))
        .collect();
    names.sort();
    names
}

fn bump(store: &Store<Cfg>, n: u32) -> Result<(), Error> {
    store.update(|cfg| {
        cfg.n = n;
        Ok(())
    })
}

/// A directory with a file in it where `target` should go, so a rename or copy onto it fails.
fn block(target: &Path) {
    fs::create_dir(target).unwrap();
    fs::write(target.join("occupied"), b"x").unwrap();
}

fn unblock(target: &Path) {
    fs::remove_file(target.join("occupied")).unwrap();
    fs::remove_dir(target).unwrap();
}

fn fixed_name() -> StoreOptions {
    StoreOptions::new(schema()).quarantine_name(QuarantineName::Fixed(".corrupt".into()))
}

#[test]
fn a_damaged_file_that_cannot_be_set_aside_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    let target = dir.path().join("s.json.corrupt");
    fs::write(&path, b"{ damaged").unwrap();
    block(&target);

    let err = Store::<Cfg>::open_path(&path, fixed_name())
        .err()
        .expect("open must fail");
    let text = err.to_string();
    assert!(
        text.contains("s.json.corrupt") && text.contains("nothing was written"),
        "{text}"
    );
    match err {
        Error::PreserveFailed {
            step,
            path: damaged,
            kept_as,
            source,
        } => {
            assert_eq!(step, PreserveStep::Quarantine);
            assert_eq!(damaged, path);
            assert_eq!(kept_as, target);
            assert!(!source.to_string().is_empty());
        }
        other => panic!("expected PreserveFailed, got {other:?}"),
    }
    assert_eq!(fs::read(&path).unwrap(), b"{ damaged");
    assert!(
        target.join("occupied").exists(),
        "what was in the way is untouched"
    );
    assert_eq!(names(dir.path()), vec!["s.json", "s.json.corrupt"]);

    // With the way clear, the same open sets the damaged file aside and starts again.
    unblock(&target);
    let store = Store::<Cfg>::open_path(&path, fixed_name()).unwrap();
    assert_eq!(store.get(), Cfg::default());
    assert_eq!(fs::read(&target).unwrap(), b"{ damaged");
}

#[test]
fn a_write_to_a_file_damaged_under_an_open_store_stops_when_it_cannot_be_set_aside() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    let target = dir.path().join("s.json.corrupt");
    let store = Store::<Cfg>::open_path(&path, fixed_name()).unwrap();
    bump(&store, 1).unwrap();
    fs::write(&path, b"{ damaged").unwrap();
    block(&target);

    let r = bump(&store, 2);
    assert!(
        matches!(
            r,
            Err(Error::PreserveFailed {
                step: PreserveStep::Quarantine,
                ..
            })
        ),
        "{r:?}"
    );
    assert_eq!(fs::read(&path).unwrap(), b"{ damaged");

    unblock(&target);
    bump(&store, 3).unwrap();
    assert_eq!(store.get().n, 3);
    assert_eq!(fs::read(&target).unwrap(), b"{ damaged");
}

#[test]
fn a_copy_of_a_damaged_file_that_cannot_be_made_stops_an_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    let target = dir.path().join("s.json.corrupt");
    fs::write(&path, b"{ damaged").unwrap();
    block(&target);
    let opts = || {
        fixed_name()
            .on_corrupt(OnCorrupt::Overwrite)
            .keep_corrupt_copy(true)
    };

    let err = Store::<Cfg>::open_path(&path, opts())
        .err()
        .expect("open must fail");
    match err {
        Error::PreserveFailed {
            step,
            path: damaged,
            kept_as,
            ..
        } => {
            assert_eq!(step, PreserveStep::CorruptCopy);
            assert_eq!(damaged, path);
            assert_eq!(kept_as, target);
        }
        other => panic!("expected PreserveFailed, got {other:?}"),
    }
    assert_eq!(fs::read(&path).unwrap(), b"{ damaged");

    unblock(&target);
    Store::<Cfg>::open_path(&path, opts()).unwrap();
    assert_eq!(
        fs::read(&target).unwrap(),
        b"{ damaged",
        "the copy was made"
    );
}

#[test]
fn a_backup_that_cannot_be_refreshed_stops_the_write_and_keeps_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    let bak = dir.path().join("s.json.bak");
    let store = Store::<Cfg>::open_path(&path, StoreOptions::new(schema())).unwrap();
    block(&bak);
    let before = fs::read(&path).unwrap();

    let r = bump(&store, 5);
    match r {
        Err(Error::PreserveFailed {
            step: PreserveStep::Backup,
            path: kept,
            kept_as,
            ..
        }) => {
            assert_eq!(kept, path);
            assert_eq!(kept_as, bak);
        }
        other => panic!("expected PreserveFailed(Backup), got {other:?}"),
    }
    assert_eq!(
        fs::read(&path).unwrap(),
        before,
        "the previous file is still there"
    );
    assert_eq!(store.get().n, 0, "the in-memory value is unchanged");

    unblock(&bak);
    bump(&store, 5).unwrap();
    assert_eq!(store.get().n, 5);
    assert!(bak.is_file(), "the backup now holds the previous file");
}

#[test]
fn a_timestamped_quarantine_never_replaces_kept_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    // Occupy every timestamped name this open could pick (one-second resolution, either side of a
    // tick) plus the first numbered retries, so the quarantine must retry past all of them.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut sentinels = Vec::new();
    for ts in now.saturating_sub(1)..=now + 2 {
        for suffix in ["".to_string(), "-1".to_string()] {
            let kept = dir.path().join(format!("s.json.corrupt-{ts}{suffix}"));
            fs::write(&kept, b"earlier evidence").unwrap();
            sentinels.push(kept);
        }
    }
    fs::write(&path, b"{ damaged").unwrap();

    let store = Store::<Cfg>::open_path(&path, StoreOptions::new(schema())).unwrap();
    assert_eq!(store.get(), Cfg::default());
    for kept in &sentinels {
        assert_eq!(fs::read(kept).unwrap(), b"earlier evidence", "{kept:?}");
    }
    let corrupt: Vec<String> = names(dir.path())
        .into_iter()
        .filter(|name| name.starts_with("s.json.corrupt-"))
        .collect();
    assert_eq!(corrupt.len(), sentinels.len() + 1, "{corrupt:?}");
    assert!(
        corrupt
            .iter()
            .any(|name| fs::read(dir.path().join(name)).unwrap() == b"{ damaged"),
        "the damaged file was kept under a fresh name: {corrupt:?}"
    );
}

#[test]
fn a_refreshed_backup_holds_the_previous_file_and_leaves_no_temp() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    let bak = dir.path().join("s.json.bak");
    let store = Store::<Cfg>::open_path(&path, StoreOptions::new(schema())).unwrap();
    bump(&store, 1).unwrap();
    let previous = fs::read(&path).unwrap();
    bump(&store, 2).unwrap();
    assert_eq!(fs::read(&bak).unwrap(), previous);
    assert!(
        names(dir.path()).iter().all(|name| !name.contains(".tmp")),
        "{:?}",
        names(dir.path())
    );
}

#[test]
fn without_sidecars_there_is_no_backup_to_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    let store =
        Store::<Cfg>::open_path(&path, StoreOptions::new(schema()).sidecars(false)).unwrap();
    block(&dir.path().join("s.json.bak"));
    bump(&store, 5).unwrap();
    assert_eq!(store.get().n, 5);
}
