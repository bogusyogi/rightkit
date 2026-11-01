//! Tests for the explorer backend. `scan_dir`/`finish_listing`/`collect_locations` run
//! without Tauri `State`, so the security-sensitive seams — hidden filtering, the
//! symlink-escape grant guard, truncation, "other" files never being granted — are
//! exercised directly against temp fixtures.

use super::{finish_listing, scan_dir, LIST_DIR_CAP};
use crate::documents::testing::StubDocs;
use crate::grants::testing::MemoryGrants;
use crate::grants::PathGrants;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Unique temp dir per fixture, removed on drop (including panic paths). Same
/// convention as `persistence/tests.rs`'s TempDbPath, but for plain directories.
struct TestDir(PathBuf);
static NEXT: AtomicU64 = AtomicU64::new(0);

impl TestDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "rightkit-explorer-test-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn display(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn list(dir: &TestDir, broker: &MemoryGrants) -> super::ExplorerListing {
    let (raws, truncated) = scan_dir(&dir.display(), LIST_DIR_CAP).unwrap();
    finish_listing(raws, truncated, broker, &StubDocs)
}

fn names(listing: &super::ExplorerListing) -> Vec<&str> {
    listing.entries.iter().map(|e| e.name.as_str()).collect()
}

#[cfg(unix)]
#[test]
fn symlinked_entries_escaping_dir_are_dropped_without_grants() {
    let outer = TestDir::new("escape-outer");
    let inner = TestDir::new("escape-inner");
    let secret = outer.path().join("secret.md");
    std::fs::write(&secret, "secret").unwrap();
    std::fs::write(outer.path().join("outside.txt"), "x").unwrap();
    // A file symlink and a dir symlink, both pointing OUTSIDE the listed dir.
    std::os::unix::fs::symlink(&secret, inner.path().join("alias.md")).unwrap();
    std::os::unix::fs::symlink(outer.path(), inner.path().join("outer-link")).unwrap();
    std::fs::write(inner.path().join("legit.md"), "ok").unwrap();

    let broker = MemoryGrants::default();
    let listing = list(&inner, &broker);
    assert_eq!(names(&listing), ["legit.md"]);
    // The escaped symlink targets never received grants — the guard drops them before
    // finish_listing can call grant_existing_file_path.
    assert!(broker
        .require_existing_file(secret.to_str().unwrap())
        .is_err());
    assert!(broker
        .require_existing_file(outer.path().join("outside.txt").to_str().unwrap())
        .is_err());
    // The real sibling was granted normally.
    assert!(broker
        .require_existing_file(&listing.entries[0].path)
        .is_ok());
}
