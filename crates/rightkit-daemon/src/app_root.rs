//! The per-app data root `<platform data dir>/Orthic Labs/<App>`, next to `Orthic Labs/shared`.
//!
//! [`AppRoot`] is built from a [`Suite`], so it honours exactly the root rules the suite already
//! has (`Suite::resolve`, `per_user_default`, `resolve_ignoring_env_overrides`, `at`,
//! `per_run_temp`); it adds no environment selector. When the suite root is the canonical
//! `.../Orthic Labs/shared`, the app root is the sibling `.../Orthic Labs/<App>`. For any other
//! suite root (an override, a test, a per-run temp) it is `<suite root>/apps/<App>`, so isolation
//! is preserved.
//!
//! Also here: path-escape-proof subpaths, a single-writer lock, and a restart-safe
//! copy-verify-commit migration from an old root. The old root is never modified.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use crate::discovery::try_lock_exclusive;
use crate::suite::{make_private_dir, Suite, SuiteError};

/// The longest app name accepted, in bytes.
pub const MAX_APP_NAME_LEN: usize = 64;
/// The commit marker written at the app root once a migration is verified.
pub const MIGRATION_MARKER: &str = ".rightkit-migrated.json";
const WRITER_LOCK: &str = ".writer.lock";
const TMP_SUFFIX: &str = ".rk-migrating";

const WINDOWS_RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Whether `s` is an app name: 1..=[`MAX_APP_NAME_LEN`] bytes of ASCII letters, digits, `-`, `_`
/// and `.`; starts and ends with a letter or digit (so no `.`, `..`, hidden or trailing-dot
/// names); no separators; not a Windows reserved device name (with or without an extension).
/// Case is preserved on disk but is significant for identity on every platform: two names that
/// differ only by case are refused as conflicting by [`AppRoot::new`] when one already exists.
pub fn valid_app_name(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() || b.len() > MAX_APP_NAME_LEN {
        return false;
    }
    if !b
        .iter()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
    {
        return false;
    }
    if !b[0].is_ascii_alphanumeric() || !b[b.len() - 1].is_ascii_alphanumeric() || s.contains("..")
    {
        return false;
    }
    // `shared` is the suite's shared-resources directory, a sibling of every app root.
    if s.eq_ignore_ascii_case("shared") {
        return false;
    }
    let stem = s.split('.').next().unwrap_or(s).to_ascii_uppercase();
    !WINDOWS_RESERVED.contains(&stem.as_str())
}

#[derive(Debug)]
pub enum AppRootError {
    InvalidName(String),
    /// A subpath was absolute, had `..`, `.`, a prefix, was empty, or resolved outside the root.
    PathEscape(String),
    /// An existing sibling differs from the requested app name only by case.
    CaseConflict {
        requested: String,
        existing: String,
    },
    /// Another process holds the single-writer lock.
    WriterBusy(PathBuf),
    /// The source tree holds a symlink or special file; refused rather than followed.
    UnsupportedSource(PathBuf),
    /// A destination file exists with different content; nothing is overwritten.
    Collision(PathBuf),
    /// Source and destination are the same tree or nested in each other.
    Overlap(PathBuf),
    /// Post-copy verification found a difference.
    VerifyFailed(PathBuf),
    Suite(SuiteError),
    Io(io::Error),
}

impl fmt::Display for AppRootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName(n) => write!(f, "invalid app name: {n:?}"),
            Self::PathEscape(p) => write!(f, "path escapes the app root: {p}"),
            Self::CaseConflict {
                requested,
                existing,
            } => write!(
                f,
                "app root {requested:?} conflicts with existing {existing:?} (differs only by case)"
            ),
            Self::WriterBusy(p) => write!(f, "another writer holds {}", p.display()),
            Self::UnsupportedSource(p) => {
                write!(
                    f,
                    "refusing symlink or special file in source: {}",
                    p.display()
                )
            }
            Self::Collision(p) => write!(f, "destination differs from source: {}", p.display()),
            Self::Overlap(p) => write!(f, "source and destination overlap: {}", p.display()),
            Self::VerifyFailed(p) => write!(f, "verification failed: {}", p.display()),
            Self::Suite(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for AppRootError {}
impl From<io::Error> for AppRootError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<SuiteError> for AppRootError {
    fn from(e: SuiteError) -> Self {
        Self::Suite(e)
    }
}

/// Held while this process is the single writer of an app root. Released on drop or process
/// death (OS lock).
pub struct WriterLock {
    _file: File,
}

/// What [`AppRoot::migrate_from`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationOutcome {
    /// Copied and verified; the marker was committed.
    Migrated { files: u64, bytes: u64 },
    /// The marker already records this source; nothing was touched.
    AlreadyMigrated,
    /// The source does not exist; nothing to do and no marker written.
    NothingToMigrate,
}

/// `<Orthic Labs>/<App>`, created private.
#[derive(Debug, Clone)]
pub struct AppRoot {
    name: String,
    root: PathBuf,
}

impl Suite {
    /// The per-app root for `app`, created private. See [`AppRoot`].
    pub fn app_root(&self, app: &str) -> Result<AppRoot, AppRootError> {
        AppRoot::new(self, app)
    }
}

impl AppRoot {
    pub fn new(suite: &Suite, app: &str) -> Result<AppRoot, AppRootError> {
        if !valid_app_name(app) {
            return Err(AppRootError::InvalidName(app.to_string()));
        }
        let parent = suite.app_parent();
        if let Ok(rd) = fs::read_dir(&parent) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if n != app && n.eq_ignore_ascii_case(app) {
                    return Err(AppRootError::CaseConflict {
                        requested: app.to_string(),
                        existing: n,
                    });
                }
            }
        }
        let root = parent.join(app);
        make_private_dir(&root)?;
        Ok(AppRoot {
            name: app.to_string(),
            root,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<app root>/<rel>` without creating anything. `rel` must be relative and made only of
    /// normal components (no `..`, `.`, root, prefix or empty); any existing ancestor that is a
    /// symlink must still resolve inside the app root.
    pub fn path(&self, rel: impl AsRef<Path>) -> Result<PathBuf, AppRootError> {
        let rel = rel.as_ref();
        let bad = || AppRootError::PathEscape(rel.display().to_string());
        let mut any = false;
        for c in rel.components() {
            match c {
                Component::Normal(n) => {
                    any = true;
                    // `:` is a Windows stream/drive separator and a reserved char elsewhere.
                    if n.to_string_lossy().contains(['\\', ':', '\0']) {
                        return Err(bad());
                    }
                }
                _ => return Err(bad()),
            }
        }
        if !any {
            return Err(bad());
        }
        let full = self.root.join(rel);
        let canon_root = fs::canonicalize(&self.root)?;
        let mut probe = full.as_path();
        loop {
            match fs::canonicalize(probe) {
                Ok(c) => {
                    if !c.starts_with(&canon_root) {
                        return Err(bad());
                    }
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    // A dangling symlink is NotFound for canonicalize but must not be followed.
                    if fs::symlink_metadata(probe).is_ok() {
                        return Err(bad());
                    }
                    match probe.parent() {
                        Some(p) if p.starts_with(&self.root) => probe = p,
                        _ => return Err(bad()),
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(full)
    }

    /// A private directory at `<app root>/<rel>`, created if absent.
    pub fn data_dir(&self, rel: impl AsRef<Path>) -> Result<PathBuf, AppRootError> {
        let d = self.path(rel)?;
        make_private_dir(&d)?;
        // Re-check after creation: nothing created may resolve outside the root.
        self.path(d.strip_prefix(&self.root).unwrap_or(&d))?;
        Ok(d)
    }

    /// Take the single-writer lock for this app root. `Err(WriterBusy)` if another holder lives.
    pub fn lock_writer(&self) -> Result<WriterLock, AppRootError> {
        let p = self.root.join(WRITER_LOCK);
        match try_lock_exclusive(&p)? {
            Some(f) => Ok(WriterLock { _file: f }),
            None => Err(AppRootError::WriterBusy(p)),
        }
    }

    /// Copy the old root `src` into this app root, verify every byte, then commit an atomic
    /// marker. Restart-safe: files land under a temporary name and are renamed only when fully
    /// written; a rerun after any interruption re-copies missing or partial files, accepts files
    /// already identical, and refuses (never overwrites) a differing destination file. The marker
    /// is written last; once present for this source the call is a no-op. `src` is never changed
    /// or deleted. Symlinks and special files in `src` are refused. Takes the writer lock.
    pub fn migrate_from(&self, src: &Path) -> Result<MigrationOutcome, AppRootError> {
        if !src.is_absolute() || src.components().any(|c| c == Component::ParentDir) {
            return Err(AppRootError::PathEscape(src.display().to_string()));
        }
        let md = match fs::symlink_metadata(src) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(MigrationOutcome::NothingToMigrate)
            }
            Err(e) => return Err(e.into()),
        };
        if !md.is_dir() {
            return Err(AppRootError::UnsupportedSource(src.to_path_buf()));
        }
        let csrc = fs::canonicalize(src)?;
        let cdst = fs::canonicalize(&self.root)?;
        if csrc.starts_with(&cdst) || cdst.starts_with(&csrc) {
            return Err(AppRootError::Overlap(src.to_path_buf()));
        }
        let _lock = self.lock_writer()?;
        let marker = self.root.join(MIGRATION_MARKER);
        if let Ok(text) = fs::read_to_string(&marker) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if v.get("source").and_then(|s| s.as_str()) == csrc.to_str() {
                    return Ok(MigrationOutcome::AlreadyMigrated);
                }
            }
        }
        let mut files = Vec::new();
        collect(&csrc, Path::new(""), &mut files)?;
        let mut total = 0u64;
        for (rel, len) in &files {
            total += len;
            let from = csrc.join(rel);
            let to = self.path(rel)?;
            if let Some(parent) = to.parent() {
                make_private_dir(parent)?;
            }
            if to.exists() {
                if same_content(&from, &to)? {
                    continue;
                }
                return Err(AppRootError::Collision(to));
            }
            let mut tmp = to.clone().into_os_string();
            tmp.push(TMP_SUFFIX);
            let tmp = PathBuf::from(tmp);
            {
                let mut r = File::open(&from)?;
                let mut w = File::create(&tmp)?;
                io::copy(&mut r, &mut w)?;
                w.sync_all()?;
            }
            fs::rename(&tmp, &to)?;
        }
        for (rel, _) in &files {
            let to = self.path(rel)?;
            if !same_content(&csrc.join(rel), &to)? {
                return Err(AppRootError::VerifyFailed(to));
            }
        }
        let body = serde_json::json!({
            "source": csrc.to_str(),
            "files": files.len(),
            "bytes": total,
        });
        let mut tmp = marker.clone().into_os_string();
        tmp.push(TMP_SUFFIX);
        let tmp = PathBuf::from(tmp);
        {
            let mut w = File::create(&tmp)?;
            w.write_all(body.to_string().as_bytes())?;
            w.sync_all()?;
        }
        fs::rename(&tmp, &marker)?;
        Ok(MigrationOutcome::Migrated {
            files: files.len() as u64,
            bytes: total,
        })
    }
}

/// Relative paths and sizes of every regular file under `base/rel`; directories recorded as
/// recursion only (empty directories are not migrated).
fn collect(base: &Path, rel: &Path, out: &mut Vec<(PathBuf, u64)>) -> Result<(), AppRootError> {
    let mut entries: Vec<_> = fs::read_dir(base.join(rel))?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name();
        if name.to_string_lossy().ends_with(TMP_SUFFIX) {
            continue;
        }
        let r = rel.join(&name);
        let ft = e.file_type()?;
        if ft.is_symlink() {
            return Err(AppRootError::UnsupportedSource(base.join(&r)));
        } else if ft.is_dir() {
            collect(base, &r, out)?;
        } else if ft.is_file() {
            out.push((r, e.metadata()?.len()));
        } else {
            return Err(AppRootError::UnsupportedSource(base.join(&r)));
        }
    }
    Ok(())
}

/// Byte-for-byte equality (stronger than a digest compare).
fn same_content(a: &Path, b: &Path) -> Result<bool, AppRootError> {
    let (ma, mb) = (fs::metadata(a)?, fs::metadata(b)?);
    if !mb.is_file() || ma.len() != mb.len() {
        return Ok(false);
    }
    let (mut fa, mut fb) = (File::open(a)?, File::open(b)?);
    let (mut ba, mut bb) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let na = read_full(&mut fa, &mut ba)?;
        let nb = read_full(&mut fb, &mut bb)?;
        if na != nb || ba[..na] != bb[..nb] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
}

fn read_full(f: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suite() -> (tempfile::TempDir, Suite) {
        let d = tempfile::tempdir().unwrap();
        let s = Suite::at(d.path().join("root")).unwrap();
        (d, s)
    }

    #[test]
    fn names_are_validated() {
        for ok in ["HeardRight", "app.orthiclabs.genright", "a", "Cut-Right_2"] {
            assert!(valid_app_name(ok), "{ok}");
        }
        let long = "a".repeat(MAX_APP_NAME_LEN + 1);
        for bad in [
            "", ".", "..", "a/b", "a\\b", "../x", "x..y", ".hidden", "trail.", "con", "NUL.txt",
            "com1", "sp ace", "a:b", "é", "shared", "Shared", &long,
        ] {
            assert!(!valid_app_name(bad), "{bad:?}");
        }
        assert!(valid_app_name(&"a".repeat(MAX_APP_NAME_LEN)));
        let (_d, s) = suite();
        assert!(matches!(
            s.app_root("../x"),
            Err(AppRootError::InvalidName(_))
        ));
    }

    #[test]
    fn canonical_suite_gives_sibling_of_shared_and_override_stays_inside() {
        let d = tempfile::tempdir().unwrap();
        let canon = Suite::at(d.path().join("Orthic Labs").join("shared")).unwrap();
        let a = canon.app_root("HeardRight").unwrap();
        assert_eq!(a.root(), d.path().join("Orthic Labs").join("HeardRight"));
        let (_d2, s) = suite();
        let a = s.app_root("HeardRight").unwrap();
        assert!(a.root().starts_with(s.root()));
        assert!(a.root().is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(a.root()).unwrap().permissions().mode() & 0o077,
                0
            );
        }
    }

    #[test]
    fn case_conflicts_are_refused() {
        let (_d, s) = suite();
        s.app_root("HeardRight").unwrap();
        assert!(matches!(
            s.app_root("heardright"),
            Err(AppRootError::CaseConflict { .. })
        ));
        s.app_root("HeardRight").unwrap();
    }

    #[test]
    fn subpath_traversal_is_refused() {
        let (_d, s) = suite();
        let a = s.app_root("App").unwrap();
        for bad in ["", ".", "..", "../x", "a/../../x", "/etc", "a:b"] {
            assert!(
                matches!(a.path(bad), Err(AppRootError::PathEscape(_))),
                "{bad:?}"
            );
        }
        // On Windows `\\` is a separator, so `a\\b` is the ordinary subpath `a/b`.
        #[cfg(unix)]
        assert!(matches!(a.path("a\\b"), Err(AppRootError::PathEscape(_))));
        let d = a.data_dir("logs/today").unwrap();
        assert!(d.is_dir() && d.starts_with(a.root()));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_refused() {
        let (d, s) = suite();
        let a = s.app_root("App").unwrap();
        let outside = d.path().join("outside");
        fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, a.root().join("link")).unwrap();
        assert!(matches!(a.path("link/x"), Err(AppRootError::PathEscape(_))));
        assert!(matches!(
            a.data_dir("link/x"),
            Err(AppRootError::PathEscape(_))
        ));
        std::os::unix::fs::symlink(d.path().join("nowhere"), a.root().join("dangling")).unwrap();
        assert!(matches!(
            a.path("dangling/x"),
            Err(AppRootError::PathEscape(_))
        ));
        assert!(!outside.join("x").exists());
    }

    #[test]
    fn writer_lock_is_single_holder() {
        let (_d, s) = suite();
        let a = s.app_root("App").unwrap();
        let l = a.lock_writer().unwrap();
        assert!(matches!(a.lock_writer(), Err(AppRootError::WriterBusy(_))));
        drop(l);
        a.lock_writer().unwrap();
    }

    fn old_tree(d: &Path) -> PathBuf {
        let old = d.join("old");
        fs::create_dir_all(old.join("sub")).unwrap();
        fs::write(old.join("a.txt"), b"alpha").unwrap();
        fs::write(old.join("sub/b.bin"), vec![7u8; 200_000]).unwrap();
        old
    }

    #[test]
    fn migration_copies_verifies_commits_and_is_idempotent() {
        let (d, s) = suite();
        let a = s.app_root("App").unwrap();
        let old = old_tree(d.path());
        assert_eq!(
            a.migrate_from(&old).unwrap(),
            MigrationOutcome::Migrated {
                files: 2,
                bytes: 200_005
            }
        );
        assert_eq!(fs::read(a.root().join("a.txt")).unwrap(), b"alpha");
        assert_eq!(fs::read(a.root().join("sub/b.bin")).unwrap().len(), 200_000);
        assert!(a.root().join(MIGRATION_MARKER).is_file());
        assert!(old.join("a.txt").is_file(), "old root is left in place");
        assert_eq!(
            a.migrate_from(&old).unwrap(),
            MigrationOutcome::AlreadyMigrated
        );
        assert_eq!(
            a.migrate_from(&d.path().join("missing")).unwrap(),
            MigrationOutcome::NothingToMigrate
        );
    }

    #[test]
    fn interrupted_migration_resumes_without_marker() {
        let (d, s) = suite();
        let a = s.app_root("App").unwrap();
        let old = old_tree(d.path());
        // Simulate a crash: a complete copy of one file and a partial temp of another.
        fs::copy(old.join("a.txt"), a.root().join("a.txt")).unwrap();
        fs::create_dir_all(a.root().join("sub")).unwrap();
        fs::write(a.root().join(format!("sub/b.bin{TMP_SUFFIX}")), b"par").unwrap();
        assert!(!a.root().join(MIGRATION_MARKER).exists());
        assert!(matches!(
            a.migrate_from(&old).unwrap(),
            MigrationOutcome::Migrated { files: 2, .. }
        ));
        assert_eq!(fs::read(a.root().join("sub/b.bin")).unwrap().len(), 200_000);
    }

    #[test]
    fn migration_refuses_collision_overlap_symlink_and_busy() {
        let (d, s) = suite();
        let a = s.app_root("App").unwrap();
        let old = old_tree(d.path());
        fs::write(a.root().join("a.txt"), b"different").unwrap();
        assert!(matches!(
            a.migrate_from(&old),
            Err(AppRootError::Collision(_))
        ));
        assert_eq!(fs::read(a.root().join("a.txt")).unwrap(), b"different");
        assert!(!a.root().join(MIGRATION_MARKER).exists());
        assert!(matches!(
            a.migrate_from(a.root()),
            Err(AppRootError::Overlap(_))
        ));
        assert!(matches!(
            a.migrate_from(Path::new("rel")),
            Err(AppRootError::PathEscape(_))
        ));
        let _held = a.lock_writer().unwrap();
        assert!(matches!(
            a.migrate_from(&old),
            Err(AppRootError::WriterBusy(_))
        ));
        #[cfg(unix)]
        {
            let b = s.app_root("Other").unwrap();
            let old2 = d.path().join("old2");
            fs::create_dir(&old2).unwrap();
            std::os::unix::fs::symlink("/etc/hosts", old2.join("l")).unwrap();
            assert!(matches!(
                b.migrate_from(&old2),
                Err(AppRootError::UnsupportedSource(_))
            ));
            assert!(!b.root().join(MIGRATION_MARKER).exists());
        }
    }
}
