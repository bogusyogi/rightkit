//! One write for RightKit's small JSON state files (notifications, trust, execution stores, schedules, ...), and
//! the one way a damaged one is set aside.
//!
//! The bytes are published with the owner's `rightkit_fs::atomic_write`, which replaces the destination NAME with a
//! new file. That is not what the plain `std::fs::write` it replaced did for a path that is a link, so the link cases
//! are decided here rather than silently changed:
//! - a symlink is followed, as `std::fs::write` followed it: the file it points at is replaced atomically and the link
//!   stays a link (a state file kept in a dotfiles repo through a symlink is not turned into a stray copy). A DANGLING
//!   link is resolved the same way: the file it names is created (as `std::fs::write` created it), and the link stays a
//!   link; a link that cannot be resolved at all (a loop) is an error, never replaced.
//! - a hard-linked file (link count above 1; Unix `nlink`, Windows `nNumberOfLinks` from
//!   `GetFileInformationByHandle`) is written IN PLACE: replacing the name would leave the other names on the old
//!   content, so the link wins over atomicity there, as it did before adoption.
//!
//! The in-place path is NOT crash-atomic. It truncates, writes and then flushes (`sync_data`): a crash or a kill after
//! the truncate and before the flush completes leaves an empty or partial file, and a concurrent reader can see that
//! state. That is the same exposure the plain `std::fs::write` had, limited to files that have a second hard link. The
//! loaders therefore never treat a file they cannot parse as "no data": see [`set_aside_damaged`].

use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Writes `bytes` to the state file at `path` (see the module notes for symlinks and hard links).
pub(crate) fn write_store_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let target = link_target(path)?;
    if hard_linked(&target) {
        return write_in_place(&target, bytes);
    }
    rightkit_fs::atomic_write(&target, bytes).map_err(io::Error::other)
}

/// Truncate, write, flush. Not crash-atomic (module notes).
fn write_in_place(target: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(target)?;
    file.write_all(bytes)?;
    file.sync_data()
}

/// The file a write to `path` must replace: when `path` is a symlink, the file the chain of links names (even if it does
/// not exist yet, which is where `std::fs::write` created it); otherwise `path`.
fn link_target(path: &Path) -> io::Result<PathBuf> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => match std::fs::canonicalize(path) {
            Ok(resolved) => Ok(resolved),
            Err(_) => resolve_dangling(path),
        },
        _ => Ok(path.to_path_buf()),
    }
}

/// Follow a symlink chain whose end does not exist: the path it would create. A chain longer than the usual OS limit
/// (a loop) is an error.
fn resolve_dangling(path: &Path) -> io::Result<PathBuf> {
    let mut current = path.to_path_buf();
    for _ in 0..40 {
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let next = std::fs::read_link(&current)?;
                current = if next.is_absolute() {
                    next
                } else {
                    current
                        .parent()
                        .map_or(next.clone(), |parent| parent.join(&next))
                };
            }
            // The end of the chain: a regular path that does not exist (or a file that appeared meanwhile).
            _ => return Ok(current),
        }
    }
    Err(io::Error::other(format!(
        "too many levels of symbolic links: {}",
        path.display()
    )))
}

#[cfg(unix)]
fn hard_linked(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.nlink() > 1)
}

/// The link count of a regular file, from `GetFileInformationByHandle` (`std` only has it on nightly).
#[cfg(windows)]
fn hard_linked(path: &Path) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    if !std::fs::metadata(path).is_ok_and(|meta| meta.is_file()) {
        return false;
    }
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    // SAFETY: `info` is a plain-data out structure we own; the handle is valid for the call (`file` is alive).
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) };
    ok != 0 && info.nNumberOfLinks > 1
}

#[cfg(not(any(unix, windows)))]
fn hard_linked(_path: &Path) -> bool {
    false
}

/// Moves a state file the app could not parse out of the way, so the next save cannot overwrite what an operator (or a
/// later build) could still repair: `<name>.damaged-<unix seconds>`, with `-1`, `-2`, ... when that name is taken (an
/// exclusive rename, so an earlier set-aside copy is never replaced). A missing, empty or whitespace-only file is not
/// worth keeping and is left alone. Returns the new path when a file was set aside. Best effort: a failed rename is
/// logged, and the caller carries on with its default.
pub(crate) fn set_aside_damaged(path: &Path) -> Option<PathBuf> {
    let text = std::fs::read(path).ok()?;
    if text.iter().all(u8::is_ascii_whitespace) {
        return None;
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let base = path.file_name()?.to_string_lossy().into_owned();
    for attempt in 0..100u32 {
        let name = if attempt == 0 {
            format!("{base}.damaged-{stamp}")
        } else {
            format!("{base}.damaged-{stamp}-{attempt}")
        };
        let to = path.with_file_name(name);
        match rightkit_fs::rename_exclusive(path, &to) {
            Ok(()) => {
                tracing::warn!(file = %path.display(), kept_as = %to.display(), "state file could not be parsed; kept aside and starting empty");
                return Some(to);
            }
            Err(rightkit_fs::FsError::AlreadyExists(_)) => continue,
            Err(error) => {
                tracing::warn!(file = %path.display(), %error, "state file could not be parsed and could not be set aside");
                return None;
            }
        }
    }
    None
}

/// Reads and parses a state file for a loader whose fallback is a default value. `None` when the file is missing or
/// unreadable (nothing to keep). A file that exists but does not parse (empty after a torn write, or partial) is set
/// aside by [`set_aside_damaged`] before `None` is returned, so the default the caller starts with is never saved over
/// the only copy of the data.
pub(crate) fn load_or_set_aside<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let raw = std::fs::read(path).ok()?;
    match serde_json::from_slice::<T>(&raw) {
        Ok(value) => Some(value),
        Err(_) => {
            set_aside_damaged(path);
            None
        }
    }
}

/// The instinct store at `path`: empty when the file is missing; when it exists but does not parse it is set aside
/// first (see [`set_aside_damaged`]), so the empty store is never saved over the only copy.
pub(crate) fn load_instincts(path: &Path) -> rightkit_adapt::InstinctStore {
    match rightkit_adapt::InstinctStore::load_from_json(&path.to_string_lossy()) {
        Ok(store) => store,
        Err(_) => {
            // Only a file we could read but not parse is damaged; a missing or unreadable one has nothing to keep.
            if std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
                set_aside_damaged(path);
            }
            rightkit_adapt::InstinctStore::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "coderight-store-write-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_plain_path_is_created_then_replaced() {
        let dir = scratch("plain");
        let file = dir.join("state.json");
        write_store_file(&file, b"one").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"one");
        write_store_file(&file, b"two").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"two");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_stays_a_link_and_its_target_is_replaced() {
        let dir = scratch("symlink");
        let target = dir.join("real.json");
        let link = dir.join("state.json");
        std::fs::write(&target, b"old").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        write_store_file(&link, b"new").unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link is still a link"
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"new",
            "the file it points at holds the new content"
        );
        assert_eq!(std::fs::read(&link).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_is_resolved_through_and_the_target_is_created() {
        let dir = scratch("dangling");
        let link = dir.join("state.json");
        let target = dir.join("missing.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        write_store_file(&link, b"new").unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link is still a link"
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"new",
            "the file it names was created, as std::fs::write did"
        );
        assert_eq!(std::fs::read(&link).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_chain_of_links_ending_nowhere_is_resolved_and_a_loop_is_an_error() {
        let dir = scratch("chain");
        let (a, b, end) = (dir.join("a.json"), dir.join("b.json"), dir.join("end.json"));
        std::os::unix::fs::symlink("b.json", &a).unwrap(); // relative, as dotfile links usually are
        std::os::unix::fs::symlink("end.json", &b).unwrap();
        write_store_file(&a, b"new").unwrap();
        assert_eq!(std::fs::read(&end).unwrap(), b"new");
        assert!(std::fs::symlink_metadata(&a)
            .unwrap()
            .file_type()
            .is_symlink());
        let (x, y) = (dir.join("x.json"), dir.join("y.json"));
        std::os::unix::fs::symlink("y.json", &x).unwrap();
        std::os::unix::fs::symlink("x.json", &y).unwrap();
        assert!(
            write_store_file(&x, b"new").is_err(),
            "a link loop is refused, not replaced"
        );
        assert!(std::fs::symlink_metadata(&x)
            .unwrap()
            .file_type()
            .is_symlink());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_hard_link_keeps_every_name_on_the_new_content() {
        let dir = scratch("hardlink");
        let first = dir.join("state.json");
        let second = dir.join("alias.json");
        std::fs::write(&first, b"old").unwrap();
        std::fs::hard_link(&first, &second).unwrap();
        write_store_file(&first, b"new").unwrap();
        assert_eq!(std::fs::read(&first).unwrap(), b"new");
        assert_eq!(
            std::fs::read(&second).unwrap(),
            b"new",
            "the other name was not left on the old content"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn a_hard_link_is_detected_and_a_lone_file_is_not() {
        let dir = scratch("detect");
        let first = dir.join("one.json");
        std::fs::write(&first, b"x").unwrap();
        assert!(!hard_linked(&first), "a lone file has link count 1");
        std::fs::hard_link(&first, dir.join("two.json")).unwrap();
        assert!(hard_linked(&first));
        assert!(!hard_linked(&dir), "a directory is never reported");
        assert!(!hard_linked(&dir.join("missing.json")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn the_in_place_path_keeps_the_inode_and_shorter_content_does_not_leave_a_tail() {
        let dir = scratch("inplace");
        let first = dir.join("state.json");
        let second = dir.join("alias.json");
        std::fs::write(&first, b"a much longer old body").unwrap();
        std::fs::hard_link(&first, &second).unwrap();
        write_store_file(&first, b"short").unwrap();
        assert_eq!(
            std::fs::read(&second).unwrap(),
            b"short",
            "truncated, not overwritten over the old tail"
        );
        assert!(hard_linked(&first), "still one file with two names");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[derive(serde::Deserialize, Debug, PartialEq)]
    struct Probe {
        items: Vec<u32>,
    }

    #[test]
    fn a_non_utf8_file_is_set_aside_not_overwritten() {
        let dir = scratch("nonutf8");
        let file = dir.join("store.json");
        std::fs::write(&file, b"{\"items\": [\xff\xfe]}").unwrap();
        assert!(load_or_set_aside::<Probe>(&file).is_none());
        assert!(
            !file.exists(),
            "invalid UTF-8 is set aside, so a save cannot overwrite it"
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_file_is_set_aside_and_never_replaced_by_the_default() {
        let dir = scratch("aside");
        let file = dir.join("store.json");
        std::fs::write(&file, br#"{"items": [1, 2"#).unwrap();
        assert!(load_or_set_aside::<Probe>(&file).is_none());
        assert!(
            !file.exists(),
            "the damaged file is out of the way, so a save cannot overwrite it"
        );
        let kept: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert!(kept[0].starts_with("store.json.damaged-"), "{kept:?}");
        assert_eq!(
            std::fs::read(dir.join(&kept[0])).unwrap(),
            br#"{"items": [1, 2"#,
            "its bytes are untouched"
        );
        // a second damaged file does not replace the first copy
        std::fs::write(&file, b"{ broken").unwrap();
        assert!(load_or_set_aside::<Probe>(&file).is_none());
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            2,
            "both copies are kept"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_instinct_store_is_set_aside_not_overwritten() {
        let dir = scratch("instincts");
        let file = dir.join("instincts.json");
        std::fs::write(&file, br#"{"instincts": [{"id": "i1""#).unwrap();
        assert_eq!(load_instincts(&file).len(), 0);
        assert!(
            !file.exists(),
            "the damaged store is kept aside, so the next save cannot replace it"
        );
        assert!(std::fs::read_dir(&dir).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("instincts.json.damaged-")));
        // a good store loads and stays
        let store = rightkit_adapt::InstinctStore::default();
        std::fs::write(&file, store.to_json().unwrap()).unwrap();
        assert_eq!(load_instincts(&file).len(), 0);
        assert!(file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_good_missing_or_empty_file_is_not_set_aside() {
        let dir = scratch("keep");
        let file = dir.join("store.json");
        assert!(
            load_or_set_aside::<Probe>(&file).is_none(),
            "missing: nothing to read"
        );
        std::fs::write(&file, br#"{"items": [7]}"#).unwrap();
        assert_eq!(
            load_or_set_aside::<Probe>(&file),
            Some(Probe { items: vec![7] })
        );
        assert!(file.exists());
        std::fs::write(&file, b"").unwrap();
        assert!(load_or_set_aside::<Probe>(&file).is_none());
        assert!(
            file.exists(),
            "an empty file holds nothing to keep and is left for the next save"
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
