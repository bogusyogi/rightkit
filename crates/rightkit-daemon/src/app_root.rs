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
//! copy-verify-commit migration from an old root. The old root is never modified. Migration is
//! strict by default (symlinks and special files are refused); [`MigrateOptions`] opts in to
//! copying symlinks as links and skipping sockets and FIFOs.

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
    /// The source tree holds a symlink or special file that this migration mode refuses (strict
    /// mode refuses every one; an opt-in mode still refuses devices and other specials).
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

impl AppRootError {
    /// The typed link failure carried by an [`AppRootError::Io`], if that is what this is: a
    /// symlink the destination could not create (Windows without the symlink privilege or
    /// Developer Mode, or a filesystem without links) during an opt-in migration. Read the link,
    /// its original target and the target that would have been written from [`LinkNotCreated`].
    pub fn link_not_created(&self) -> Option<&LinkNotCreated> {
        match self {
            Self::Io(e) => e.get_ref()?.downcast_ref::<LinkNotCreated>(),
            _ => None,
        }
    }

    /// True when trying again can succeed without changing the request: a link the destination
    /// could not create yet ([`AppRootError::link_not_created`]) or a busy writer lock.
    pub fn is_retryable(&self) -> bool {
        self.link_not_created().is_some() || matches!(self, Self::WriterBusy(_))
    }
}

/// A required symlink that the destination could not create. Carried inside
/// [`AppRootError::Io`]; read it with [`AppRootError::link_not_created`].
///
/// Retryable: it is returned before the completion marker is written, so the destination is not
/// treated as migrated, the old tree is untouched and still usable, and a later call (after the
/// privilege is granted) resumes and completes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LinkNotCreated {
    /// Path of the link relative to the migrated tree.
    pub link: PathBuf,
    /// The link's target text in the old tree.
    pub target: PathBuf,
    /// The target that would have been written (absolute for a rewritten external link).
    pub resolved: PathBuf,
    /// The operating system's reason.
    pub reason: String,
}

impl fmt::Display for LinkNotCreated {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cannot create symlink {} -> {} in the app root: {}",
            self.link.display(),
            self.target.display(),
            self.reason
        )
    }
}
impl std::error::Error for LinkNotCreated {}

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

/// What [`AppRoot::migrate_from_with`] did: the [`MigrationOutcome`] plus what an opt-in
/// migration did with links and special files. The lists describe this call's work only (they are
/// empty when the outcome is `AlreadyMigrated` or `NothingToMigrate`; the marker holds the record
/// of an earlier call).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MigrationReport {
    outcome: MigrationOutcome,
    migrated_links: Vec<MigratedLink>,
    skipped_specials: Vec<SkippedSpecial>,
    omitted_optional_links: Vec<OmittedLink>,
    complete: bool,
}

impl MigrationReport {
    fn bare(outcome: MigrationOutcome, complete: bool) -> Self {
        Self {
            outcome,
            migrated_links: Vec::new(),
            skipped_specials: Vec::new(),
            omitted_optional_links: Vec::new(),
            complete,
        }
    }

    /// The same value `migrate_from` returns.
    pub fn outcome(&self) -> &MigrationOutcome {
        &self.outcome
    }

    pub fn into_outcome(self) -> MigrationOutcome {
        self.outcome
    }

    /// Symlinks recreated in the app root (as link entries, never followed).
    pub fn migrated_links(&self) -> &[MigratedLink] {
        &self.migrated_links
    }

    /// Sockets and FIFOs skipped (see [`MigrateOptions::skip_ephemeral_specials`]).
    pub fn skipped_specials(&self) -> &[SkippedSpecial] {
        &self.skipped_specials
    }

    /// Links the caller's policy marked optional and that could not be created. A required link
    /// that cannot be created is an error instead ([`AppRootError::link_not_created`]).
    pub fn omitted_optional_links(&self) -> &[OmittedLink] {
        &self.omitted_optional_links
    }

    /// False when an optional link was omitted (the marker records the same value). Skipped
    /// ephemeral specials do not make a migration incomplete. For `AlreadyMigrated` this is the
    /// value the earlier marker recorded.
    pub fn complete(&self) -> bool {
        self.complete
    }
}

/// A symlink recreated by a link-preserving migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigratedLink {
    /// Path of the link relative to the migrated tree.
    pub path: PathBuf,
    /// The link's target text in the old tree, exactly as `read_link` returned it.
    pub original_target: PathBuf,
    /// The target text written to the new link.
    pub written_target: PathBuf,
    /// True when a relative link leaving the tree was rewritten to an absolute path to its
    /// original target.
    pub rewritten: bool,
}

/// The kind of ephemeral special file skipped by a migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecialKind {
    Socket,
    Fifo,
}

impl SpecialKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Socket => "socket",
            Self::Fifo => "fifo",
        }
    }
}

/// A socket or FIFO that was skipped (not opened, not copied).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedSpecial {
    /// Path relative to the migrated tree.
    pub path: PathBuf,
    pub kind: SpecialKind,
}

/// An optional link that the destination could not hold and the caller's policy allowed to omit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmittedLink {
    /// Path of the link relative to the migrated tree.
    pub path: PathBuf,
    /// The link's target text in the old tree.
    pub original_target: PathBuf,
    /// The target that would have been written (absolute for a rewritten external link).
    pub resolved_target: PathBuf,
    /// Why the link could not be created.
    pub reason: String,
}

type OptionalLinkPolicy = std::sync::Arc<dyn Fn(&Path) -> bool + Send + Sync>;

/// Opt-in behaviour for [`AppRoot::migrate_from_with`]. The default is strict and identical to
/// [`AppRoot::migrate_from`]: any symlink or special file in the source is refused.
///
/// The type is `#[non_exhaustive]`; build it with [`MigrateOptions::strict`] and the builders.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct MigrateOptions {
    links: bool,
    ephemeral_specials: bool,
    optional_link: Option<OptionalLinkPolicy>,
}

impl fmt::Debug for MigrateOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MigrateOptions")
            .field("links", &self.links)
            .field("ephemeral_specials", &self.ephemeral_specials)
            .field("optional_link_policy", &self.optional_link.is_some())
            .finish()
    }
}

impl MigrateOptions {
    /// Refuse every symlink and special file (the default).
    pub fn strict() -> Self {
        Self::default()
    }

    /// Copy symlinks as link entries, never traversing them. A relative link that stays inside
    /// the tree keeps its text; a relative link that leaves the tree becomes an absolute link to
    /// its original target (resolved lexically against the link's old location); an absolute or
    /// dangling link is copied as it is. If the destination cannot create a link the migration
    /// fails with the retryable [`AppRootError::link_not_created`] error before any marker is written,
    /// unless [`MigrateOptions::optional_links`] marks that link omittable.
    pub fn preserve_links(mut self, on: bool) -> Self {
        self.links = on;
        self
    }

    /// Skip sockets and FIFOs (never opened) and report them. Device nodes and any other special
    /// file are still refused.
    pub fn skip_ephemeral_specials(mut self, on: bool) -> Self {
        self.ephemeral_specials = on;
        self
    }

    /// Caller policy naming links that may be omitted when the destination cannot create them.
    /// The predicate receives the link's path relative to the migrated tree. It only matters
    /// together with [`MigrateOptions::preserve_links`], and only for a link whose creation
    /// failed; every other link is migrated normally. Omissions are reported in the outcome and
    /// in the marker, separately from required links.
    pub fn optional_links(
        mut self,
        is_optional: impl Fn(&Path) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.optional_link = Some(std::sync::Arc::new(is_optional));
        self
    }

    pub fn links(&self) -> bool {
        self.links
    }

    pub fn ephemeral_specials(&self) -> bool {
        self.ephemeral_specials
    }

    fn is_strict(&self) -> bool {
        !self.links && !self.ephemeral_specials && self.optional_link.is_none()
    }

    fn is_optional(&self, rel: &Path) -> bool {
        self.optional_link.as_ref().is_some_and(|f| f(rel))
    }
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
        self.path_checked(rel.as_ref(), true)
    }

    /// [`AppRoot::path`]; with `follow_leaf` false only the ancestors are validated and the final
    /// component may be a symlink (a link entry being placed) that is never resolved.
    fn path_checked(&self, rel: &Path, follow_leaf: bool) -> Result<PathBuf, AppRootError> {
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
        let mut probe = if follow_leaf {
            full.as_path()
        } else {
            full.parent().unwrap_or(&self.root)
        };
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
    /// or deleted. Symlinks and special files in `src` are refused (strict); use
    /// [`AppRoot::migrate_from_with`] to opt in to links and ephemeral specials. Takes the writer
    /// lock.
    pub fn migrate_from(&self, src: &Path) -> Result<MigrationOutcome, AppRootError> {
        Ok(self
            .migrate_from_with(src, MigrateOptions::strict())?
            .into_outcome())
    }

    /// [`AppRoot::migrate_from`] with opt-in behaviour. Lock, restart, collision handling and the
    /// marker-last ordering are the same in every mode; `MigrateOptions::strict()` is exactly
    /// `migrate_from`. See [`MigrateOptions`] for how links and ephemeral specials are handled.
    /// Returns a [`MigrationReport`] (its `outcome()` is what `migrate_from` returns).
    /// A required link that the destination cannot create returns the retryable
    /// [`AppRootError::link_not_created`] error before the marker is written: the old tree is untouched
    /// and the incomplete destination has no marker, so it is never treated as migrated.
    pub fn migrate_from_with(
        &self,
        src: &Path,
        opts: MigrateOptions,
    ) -> Result<MigrationReport, AppRootError> {
        self.migrate_impl(src, &opts, &make_link)
    }

    fn migrate_impl(
        &self,
        src: &Path,
        opts: &MigrateOptions,
        maker: LinkMaker<'_>,
    ) -> Result<MigrationReport, AppRootError> {
        if !src.is_absolute() || src.components().any(|c| c == Component::ParentDir) {
            return Err(AppRootError::PathEscape(src.display().to_string()));
        }
        let md = match fs::symlink_metadata(src) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(MigrationReport::bare(
                    MigrationOutcome::NothingToMigrate,
                    true,
                ))
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
                    let complete = v.get("complete").and_then(|c| c.as_bool()).unwrap_or(true);
                    return Ok(MigrationReport::bare(
                        MigrationOutcome::AlreadyMigrated,
                        complete,
                    ));
                }
            }
        }
        let mut entries = Vec::new();
        collect(&csrc, Path::new(""), opts, &mut entries)?;
        let mut nfiles = 0u64;
        let mut total = 0u64;
        let mut links: Vec<MigratedLink> = Vec::new();
        // (relative path, written target, is a directory link) for verification.
        let mut created: Vec<(PathBuf, PathBuf, bool)> = Vec::new();
        let mut specials: Vec<SkippedSpecial> = Vec::new();
        let mut omitted: Vec<OmittedLink> = Vec::new();
        for entry in &entries {
            match entry {
                Entry::File { rel, len } => {
                    nfiles += 1;
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
                Entry::Link { rel, target, dir } => {
                    let (written, rewritten) = plan_link(&csrc, rel, target);
                    // Validate the parent chain only: the leaf is a link and is never followed.
                    let to = self.path_checked(rel, false)?;
                    if let Some(parent) = to.parent() {
                        make_private_dir(parent)?;
                    }
                    match fs::symlink_metadata(&to) {
                        Ok(m) => {
                            if !(m.file_type().is_symlink()
                                && same_link_target(&fs::read_link(&to)?, &written))
                            {
                                return Err(AppRootError::Collision(to));
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {
                            if let Err(e) = maker(&written, &to, *dir) {
                                if !link_unavailable(&e) {
                                    return Err(e.into());
                                }
                                if opts.is_optional(rel) {
                                    omitted.push(OmittedLink {
                                        path: rel.clone(),
                                        original_target: target.clone(),
                                        resolved_target: written,
                                        reason: e.to_string(),
                                    });
                                    continue;
                                }
                                return Err(AppRootError::Io(io::Error::other(LinkNotCreated {
                                    link: rel.clone(),
                                    target: target.clone(),
                                    resolved: written,
                                    reason: e.to_string(),
                                })));
                            }
                        }
                        Err(e) => return Err(e.into()),
                    }
                    created.push((rel.clone(), written.clone(), *dir));
                    links.push(MigratedLink {
                        path: rel.clone(),
                        original_target: target.clone(),
                        written_target: written,
                        rewritten,
                    });
                }
                Entry::Special { rel, kind } => specials.push(SkippedSpecial {
                    path: rel.clone(),
                    kind: *kind,
                }),
            }
        }
        for entry in &entries {
            if let Entry::File { rel, .. } = entry {
                let to = self.path(rel)?;
                if !same_content(&csrc.join(rel), &to)? {
                    return Err(AppRootError::VerifyFailed(to));
                }
            }
        }
        for (rel, written, dir) in &created {
            // Compare with read_link only; a link is never followed.
            let to = self.path_checked(rel, false)?;
            let ok = match fs::symlink_metadata(&to) {
                Ok(m) => {
                    m.file_type().is_symlink()
                        && fs::read_link(&to).is_ok_and(|t| same_link_target(&t, written))
                        && link_kind_matches(&m.file_type(), *dir)
                }
                Err(_) => false,
            };
            if !ok {
                return Err(AppRootError::VerifyFailed(to));
            }
        }
        let body = marker_json(
            csrc.to_str(),
            nfiles,
            total,
            opts,
            &links,
            &specials,
            &omitted,
        );
        let mut tmp = marker.clone().into_os_string();
        tmp.push(TMP_SUFFIX);
        let tmp = PathBuf::from(tmp);
        {
            let mut w = File::create(&tmp)?;
            w.write_all(body.to_string().as_bytes())?;
            w.sync_all()?;
        }
        fs::rename(&tmp, &marker)?;
        let complete = omitted.is_empty();
        Ok(MigrationReport {
            outcome: MigrationOutcome::Migrated {
                files: nfiles,
                bytes: total,
            },
            migrated_links: links,
            skipped_specials: specials,
            omitted_optional_links: omitted,
            complete,
        })
    }
}

type LinkMaker<'a> = &'a dyn Fn(&Path, &Path, bool) -> io::Result<()>;

/// One thing found under the source tree.
enum Entry {
    File {
        rel: PathBuf,
        len: u64,
    },
    /// A symlink, recorded without following it. `dir` is true for a Windows directory link.
    Link {
        rel: PathBuf,
        target: PathBuf,
        dir: bool,
    },
    Special {
        rel: PathBuf,
        kind: SpecialKind,
    },
}

/// Every regular file, and under `opts` every symlink and ephemeral special, below `base/rel`;
/// directories are recursion only (empty directories are not migrated). A symlink is never
/// traversed. Without the matching option a symlink or special file is refused.
fn collect(
    base: &Path,
    rel: &Path,
    opts: &MigrateOptions,
    out: &mut Vec<Entry>,
) -> Result<(), AppRootError> {
    let mut entries: Vec<_> = fs::read_dir(base.join(rel))?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name();
        if name.to_string_lossy().ends_with(TMP_SUFFIX) {
            continue;
        }
        let r = rel.join(&name);
        // DirEntry::file_type does not follow symlinks.
        let ft = e.file_type()?;
        if ft.is_symlink() {
            if !opts.links {
                return Err(AppRootError::UnsupportedSource(base.join(&r)));
            }
            let target = fs::read_link(base.join(&r))?;
            out.push(Entry::Link {
                dir: link_is_dir(&ft),
                rel: r,
                target,
            });
        } else if ft.is_dir() {
            collect(base, &r, opts, out)?;
        } else if ft.is_file() {
            out.push(Entry::File {
                len: e.metadata()?.len(),
                rel: r,
            });
        } else {
            match special_kind(&ft) {
                // Never opened, never copied.
                Some(kind) if opts.ephemeral_specials => out.push(Entry::Special { rel: r, kind }),
                _ => return Err(AppRootError::UnsupportedSource(base.join(&r))),
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn special_kind(ft: &fs::FileType) -> Option<SpecialKind> {
    use std::os::unix::fs::FileTypeExt;
    if ft.is_socket() {
        Some(SpecialKind::Socket)
    } else if ft.is_fifo() {
        Some(SpecialKind::Fifo)
    } else {
        None
    }
}

/// Windows has no socket or FIFO file type that std can name; every special stays refused.
#[cfg(not(unix))]
fn special_kind(_ft: &fs::FileType) -> Option<SpecialKind> {
    None
}

#[cfg(windows)]
fn link_is_dir(ft: &fs::FileType) -> bool {
    use std::os::windows::fs::FileTypeExt;
    ft.is_symlink_dir()
}

#[cfg(not(windows))]
fn link_is_dir(_ft: &fs::FileType) -> bool {
    false
}

#[cfg(windows)]
fn link_kind_matches(ft: &fs::FileType, dir: bool) -> bool {
    use std::os::windows::fs::FileTypeExt;
    ft.is_symlink_dir() == dir
}

#[cfg(not(windows))]
fn link_kind_matches(_ft: &fs::FileType, _dir: bool) -> bool {
    true
}

/// The text to write for the link at `rel` whose old target text is `target`, and whether it was
/// rewritten. A target with a root or prefix is absolute and copied as it is. A relative target
/// that never climbs above the tree root keeps its text. A relative target that does climb out is
/// resolved lexically against the link's old location (`csrc/<parent of rel>`) and written as an
/// absolute path, so it keeps pointing at the original target. A target that leaves the tree and
/// comes back (`../<tree name>/x`) counts as leaving: the same text would mean something else
/// under the new root.
fn plan_link(csrc: &Path, rel: &Path, target: &Path) -> (PathBuf, bool) {
    if target
        .components()
        .any(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
    {
        return (target.to_path_buf(), false);
    }
    let parent_rel = rel.parent().unwrap_or_else(|| Path::new(""));
    let mut depth = parent_rel.components().count();
    let mut inside = true;
    let mut abs = csrc.to_path_buf();
    if depth > 0 {
        abs.push(parent_rel);
    }
    for c in target.components() {
        match c {
            Component::Normal(n) => {
                abs.push(n);
                depth += 1;
            }
            Component::ParentDir => {
                if depth == 0 {
                    inside = false;
                } else {
                    depth -= 1;
                }
                abs.pop();
            }
            _ => {}
        }
    }
    if inside {
        (target.to_path_buf(), false)
    } else {
        (strip_verbatim(abs), true)
    }
}

/// `\\?\C:\x` becomes `C:\x` and `\\?\UNC\h\s` becomes `\\h\s`, so a rewritten link is a plain
/// path. Other platforms and other forms are returned unchanged.
#[cfg(windows)]
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy().into_owned();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        if rest.len() >= 2 && rest.as_bytes()[1] == b':' {
            return PathBuf::from(rest);
        }
        p
    } else {
        p
    }
}

#[cfg(not(windows))]
fn strip_verbatim(p: PathBuf) -> PathBuf {
    p
}

/// Whether two link target texts name the same target. Exact on Unix. On Windows the stored text
/// may come back with `/` as `\`, a verbatim prefix or another case, so those are normalised.
fn same_link_target(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    #[cfg(windows)]
    {
        fn norm(p: &Path) -> String {
            let s = p.to_string_lossy().replace('/', "\\");
            let s = s
                .strip_prefix(r"\\?\UNC\")
                .map(|r| format!(r"\\{r}"))
                .or_else(|| s.strip_prefix(r"\\?\").map(str::to_string))
                .or_else(|| s.strip_prefix(r"\??\").map(str::to_string))
                .unwrap_or(s);
            s.to_lowercase()
        }
        norm(a) == norm(b)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Create a symlink at `to` pointing at `target`. `dir` selects `symlink_dir` on Windows.
fn make_link(target: &Path, to: &Path, dir: bool) -> io::Result<()> {
    #[cfg(unix)]
    {
        let _ = dir;
        std::os::unix::fs::symlink(target, to)
    }
    #[cfg(windows)]
    {
        if dir {
            std::os::windows::fs::symlink_dir(target, to)
        } else {
            std::os::windows::fs::symlink_file(target, to)
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, to, dir);
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

/// Whether a link-creation error means the destination cannot hold links (Windows without
/// SeCreateSymbolicLink or Developer Mode; a filesystem without symlinks) rather than a fault
/// that should surface as plain I/O. On Windows every error except `AlreadyExists` counts.
fn link_unavailable(e: &io::Error) -> bool {
    #[cfg(windows)]
    {
        e.kind() != io::ErrorKind::AlreadyExists
    }
    #[cfg(not(windows))]
    {
        matches!(
            e.kind(),
            io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
        )
    }
}

fn lossy(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The commit marker body. A strict migration writes exactly the historical shape
/// (`source`, `files`, `bytes`); an opt-in one adds `links`, `skipped_specials`,
/// `omitted_optional_links` and `complete` (false while any optional link was omitted). Readers
/// of the old shape look only at `source`, so both shapes stay readable by both versions.
fn marker_json(
    source: Option<&str>,
    files: u64,
    bytes: u64,
    opts: &MigrateOptions,
    links: &[MigratedLink],
    specials: &[SkippedSpecial],
    omitted: &[OmittedLink],
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "source": source,
        "files": files,
        "bytes": bytes,
    });
    if opts.is_strict() {
        return body;
    }
    let obj = body.as_object_mut().expect("object");
    obj.insert(
        "links".into(),
        links
            .iter()
            .map(|l| {
                serde_json::json!({
                    "path": lossy(&l.path),
                    "original_target": lossy(&l.original_target),
                    "written_target": lossy(&l.written_target),
                    "rewritten": l.rewritten,
                })
            })
            .collect(),
    );
    obj.insert(
        "skipped_specials".into(),
        specials
            .iter()
            .map(|s| serde_json::json!({"path": lossy(&s.path), "kind": s.kind.as_str()}))
            .collect(),
    );
    obj.insert(
        "omitted_optional_links".into(),
        omitted
            .iter()
            .map(|o| {
                serde_json::json!({
                    "path": lossy(&o.path),
                    "original_target": lossy(&o.original_target),
                    "resolved_target": lossy(&o.resolved_target),
                    "reason": o.reason,
                })
            })
            .collect(),
    );
    obj.insert(
        "complete".into(),
        serde_json::Value::Bool(omitted.is_empty()),
    );
    body
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
    /// flock (Unix `File::try_lock`) belongs to the open file description, which a child forked by another test in
    /// this binary shares until it execs (child-process spawns with pre_exec take the fork path on macOS). A lock
    /// released by drop can therefore read as held for that instant: re-acquire with a short bound. Not a release bug:
    /// the guard's drop closes our descriptor; the lock ends when the last shared copy closes (#788).
    fn eventually<T, E>(mut f: impl FnMut() -> Result<T, E>) -> Result<T, E> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match f() {
                Ok(v) => return Ok(v),
                Err(e) if std::time::Instant::now() >= deadline => return Err(e),
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(20)),
            }
        }
    }

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
        let _held = eventually(|| a.lock_writer()).unwrap();
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

    /// Every entry under `root` without following links: (kind + relative path, payload), where the
    /// payload is file bytes or the link's target text.
    fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
        fn walk(base: &Path, rel: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            let mut names: Vec<_> = fs::read_dir(base.join(rel))
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            names.sort();
            for n in names {
                let r = rel.join(&n);
                let full = base.join(&r);
                let md = fs::symlink_metadata(&full).unwrap();
                let key = r.display().to_string();
                if md.file_type().is_symlink() {
                    let t = fs::read_link(&full).unwrap();
                    out.push((
                        format!("L {key}"),
                        t.to_string_lossy().into_owned().into_bytes(),
                    ));
                } else if md.is_dir() {
                    out.push((format!("D {key}"), Vec::new()));
                    walk(base, &r, out);
                } else if md.is_file() {
                    out.push((format!("F {key}"), fs::read(&full).unwrap()));
                } else {
                    out.push((format!("O {key}"), Vec::new()));
                }
            }
        }
        let mut out = Vec::new();
        walk(root, Path::new(""), &mut out);
        out
    }

    fn links_on() -> MigrateOptions {
        MigrateOptions::strict().preserve_links(true)
    }

    fn marker_value(a: &AppRoot) -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(a.root().join(MIGRATION_MARKER)).unwrap()).unwrap()
    }

    #[test]
    fn strict_marker_keeps_the_historical_shape_and_old_markers_still_count() {
        let (d, s) = suite();
        let a = s.app_root("App").unwrap();
        let old = old_tree(d.path());
        a.migrate_from(&old).unwrap();
        let v = marker_value(&a);
        let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["bytes", "files", "source"]);
        // An opt-in call honours a marker written by a strict (old-format) migration.
        assert_eq!(
            a.migrate_from_with(&old, links_on().skip_ephemeral_specials(true))
                .unwrap()
                .outcome(),
            &MigrationOutcome::AlreadyMigrated
        );
        // An opt-in migration of a plain tree adds the new keys, all empty, and is complete.
        let b = s.app_root("Other").unwrap();
        let out = b.migrate_from_with(&old, links_on()).unwrap();
        assert_eq!(
            out.outcome(),
            &MigrationOutcome::Migrated {
                files: 2,
                bytes: 200_005
            }
        );
        assert!(out.migrated_links().is_empty());
        assert!(out.skipped_specials().is_empty());
        assert!(out.omitted_optional_links().is_empty());
        assert!(out.complete());
        let v = marker_value(&b);
        assert_eq!(v["links"], serde_json::json!([]));
        assert_eq!(v["skipped_specials"], serde_json::json!([]));
        assert_eq!(v["omitted_optional_links"], serde_json::json!([]));
        assert_eq!(v["complete"], serde_json::json!(true));
        assert_eq!(v["files"], serde_json::json!(2));
    }

    #[test]
    fn marker_json_reports_omitted_links_as_incomplete() {
        let omitted = [OmittedLink {
            path: PathBuf::from("skills/x"),
            original_target: PathBuf::from("../../x"),
            resolved_target: PathBuf::from("/abs/x"),
            reason: "denied".into(),
        }];
        let v = marker_json(Some("/old"), 1, 2, &links_on(), &[], &[], &omitted);
        assert_eq!(v["complete"], serde_json::json!(false));
        assert_eq!(v["omitted_optional_links"][0]["path"], "skills/x");
        assert_eq!(v["omitted_optional_links"][0]["original_target"], "../../x");
        assert_eq!(v["omitted_optional_links"][0]["resolved_target"], "/abs/x");
    }

    #[test]
    fn link_creation_errors_are_classified() {
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        assert!(link_unavailable(&denied));
        assert!(link_unavailable(&io::Error::from(
            io::ErrorKind::Unsupported
        )));
        assert!(!link_unavailable(&io::Error::from(
            io::ErrorKind::AlreadyExists
        )));
        let typed = LinkNotCreated {
            link: "a".into(),
            target: "b".into(),
            resolved: "b".into(),
            reason: "denied".into(),
        };
        let e = AppRootError::Io(io::Error::other(typed.clone()));
        assert_eq!(e.link_not_created(), Some(&typed));
        assert!(e.is_retryable());
        assert!(e.to_string().contains("cannot create symlink a -> b"));
        assert!(AppRootError::WriterBusy("x".into()).is_retryable());
        assert!(!AppRootError::Collision("x".into()).is_retryable());
        // A plain I/O error is not the typed failure and is not retryable.
        let plain = AppRootError::Io(io::Error::from(io::ErrorKind::PermissionDenied));
        assert!(plain.link_not_created().is_none() && !plain.is_retryable());
    }

    #[test]
    fn plan_link_keeps_inside_text_and_resolves_outside_lexically() {
        let csrc = Path::new(if cfg!(windows) {
            r"C:\data\old"
        } else {
            "/data/old"
        });
        let rel = |s: &str| PathBuf::from(s);
        // Stays inside: same text.
        for (at, t) in [
            ("l", "a.txt"),
            ("sub/l", "../a.txt"),
            ("sub/l", "."),
            ("sub/l", ".."),
            ("l", "./x/../y"),
        ] {
            assert_eq!(
                plan_link(csrc, &rel(at), Path::new(t)),
                (PathBuf::from(t), false),
                "{at} {t}"
            );
        }
        // Leaves the tree, including leaving and re-entering.
        let (w, rewritten) = plan_link(csrc, &rel("sub/l"), Path::new("../../out/f"));
        assert!(rewritten);
        assert_eq!(w, csrc.parent().unwrap().join("out").join("f"));
        let (w, rewritten) = plan_link(csrc, &rel("l"), Path::new("../old/a.txt"));
        assert!(rewritten);
        assert_eq!(w, csrc.join("a.txt"));
    }

    #[cfg(unix)]
    mod unix_links {
        use super::*;
        use std::os::unix::fs::symlink;

        fn migrated(
            r: MigrationReport,
        ) -> (
            u64,
            Vec<MigratedLink>,
            Vec<SkippedSpecial>,
            Vec<OmittedLink>,
        ) {
            let files = match r.outcome() {
                MigrationOutcome::Migrated { files, .. } => *files,
                other => panic!("not migrated: {other:?}"),
            };
            (
                files,
                r.migrated_links().to_vec(),
                r.skipped_specials().to_vec(),
                r.omitted_optional_links().to_vec(),
            )
        }

        fn read_link_text(p: PathBuf) -> PathBuf {
            fs::read_link(p).unwrap()
        }

        #[test]
        fn internal_relative_link_keeps_its_text_and_meaning() {
            let (d, s) = suite();
            let a = s.app_root("App").unwrap();
            let old = old_tree(d.path());
            symlink("../a.txt", old.join("sub/to_a")).unwrap();
            symlink("sub", old.join("top")).unwrap();
            symlink("sub/b.bin", old.join("to_b")).unwrap();
            let before = snapshot(&old);
            let (files, links, specials, omitted) =
                migrated(a.migrate_from_with(&old, links_on()).unwrap());
            assert_eq!(files, 2);
            assert_eq!(links.len(), 3);
            assert!(specials.is_empty() && omitted.is_empty());
            assert!(links
                .iter()
                .all(|l| !l.rewritten && l.original_target == l.written_target));
            assert_eq!(
                read_link_text(a.root().join("sub/to_a")),
                Path::new("../a.txt")
            );
            assert_eq!(fs::read(a.root().join("sub/to_a")).unwrap(), b"alpha");
            assert_eq!(read_link_text(a.root().join("top")), Path::new("sub"));
            assert_eq!(fs::read(a.root().join("top/b.bin")).unwrap().len(), 200_000);
            assert_eq!(
                read_link_text(a.root().join("to_b")),
                Path::new("sub/b.bin")
            );
            let v = marker_value(&a);
            assert_eq!(v["links"].as_array().unwrap().len(), 3);
            assert_eq!(v["complete"], serde_json::json!(true));
            assert_eq!(snapshot(&old), before, "old tree is byte-identical");
        }

        #[test]
        fn external_relative_link_is_rewritten_to_its_original_target() {
            let (d, s) = suite();
            let a = s.app_root("App").unwrap();
            let old = old_tree(d.path());
            let outside = d.path().join("outside");
            fs::create_dir(&outside).unwrap();
            fs::write(outside.join("file"), b"out").unwrap();
            symlink("../../outside/file", old.join("sub/ext")).unwrap();
            // Leaves the tree and comes back: the same text would mean something else.
            symlink("../old/a.txt", old.join("reenter")).unwrap();
            let before = snapshot(&old);
            let (_, links, _, _) = migrated(a.migrate_from_with(&old, links_on()).unwrap());
            let canon_d = fs::canonicalize(d.path()).unwrap();
            let want_ext = canon_d.join("outside").join("file");
            let want_re = fs::canonicalize(&old).unwrap().join("a.txt");
            assert_eq!(read_link_text(a.root().join("sub/ext")), want_ext);
            assert_eq!(read_link_text(a.root().join("reenter")), want_re);
            assert_eq!(fs::read(a.root().join("sub/ext")).unwrap(), b"out");
            assert_eq!(fs::read(a.root().join("reenter")).unwrap(), b"alpha");
            let ext = links
                .iter()
                .find(|l| l.path == Path::new("sub/ext"))
                .unwrap();
            assert!(ext.rewritten);
            assert_eq!(ext.original_target, Path::new("../../outside/file"));
            assert_eq!(ext.written_target, want_ext);
            assert_eq!(snapshot(&old), before);
        }

        #[test]
        fn absolute_link_is_copied_verbatim() {
            let (d, s) = suite();
            let a = s.app_root("App").unwrap();
            let old = old_tree(d.path());
            let outside = d.path().join("outside");
            fs::create_dir(&outside).unwrap();
            fs::write(outside.join("file"), b"out").unwrap();
            let target = outside.join("file");
            symlink(&target, old.join("abs")).unwrap();
            let (_, links, _, _) = migrated(a.migrate_from_with(&old, links_on()).unwrap());
            assert_eq!(read_link_text(a.root().join("abs")), target);
            assert!(!links[0].rewritten && links[0].written_target == target);
        }

        #[test]
        fn dangling_links_stay_links() {
            let (d, s) = suite();
            let a = s.app_root("App").unwrap();
            let old = old_tree(d.path());
            symlink("missing/nowhere", old.join("dang")).unwrap();
            symlink("/nonexistent-rk-test/xyz", old.join("dang_abs")).unwrap();
            let before = snapshot(&old);
            let (_, links, _, _) = migrated(a.migrate_from_with(&old, links_on()).unwrap());
            assert_eq!(links.len(), 2);
            for (name, text) in [
                ("dang", "missing/nowhere"),
                ("dang_abs", "/nonexistent-rk-test/xyz"),
            ] {
                let p = a.root().join(name);
                assert!(fs::symlink_metadata(&p).unwrap().file_type().is_symlink());
                assert!(!p.exists(), "still dangling");
                assert_eq!(read_link_text(p), Path::new(text));
            }
            assert_eq!(snapshot(&old), before);
        }

        #[test]
        fn link_cycles_are_not_traversed() {
            let (d, s) = suite();
            let a = s.app_root("App").unwrap();
            let old = old_tree(d.path());
            symlink(".", old.join("loop")).unwrap();
            symlink("b", old.join("ca")).unwrap();
            symlink("ca", old.join("b")).unwrap();
            symlink("..", old.join("sub/up")).unwrap();
            let before = snapshot(&old);
            let (files, links, _, _) = migrated(a.migrate_from_with(&old, links_on()).unwrap());
            assert_eq!(files, 2, "no file was reached through a link");
            assert_eq!(links.len(), 4);
            assert_eq!(read_link_text(a.root().join("loop")), Path::new("."));
            assert_eq!(read_link_text(a.root().join("sub/up")), Path::new(".."));
            assert_eq!(read_link_text(a.root().join("ca")), Path::new("b"));
            assert_eq!(read_link_text(a.root().join("b")), Path::new("ca"));
            assert_eq!(snapshot(&old), before);
        }

        #[test]
        fn collisions_behave_as_before_with_links_enabled() {
            let (d, s) = suite();
            let old = old_tree(d.path());
            symlink("../a.txt", old.join("sub/to_a")).unwrap();
            // A differing file.
            let a = s.app_root("A").unwrap();
            fs::write(a.root().join("a.txt"), b"different").unwrap();
            assert!(matches!(
                a.migrate_from_with(&old, links_on()),
                Err(AppRootError::Collision(_))
            ));
            assert_eq!(fs::read(a.root().join("a.txt")).unwrap(), b"different");
            assert!(!a.root().join(MIGRATION_MARKER).exists());
            // A link with another target, and a regular file where a link belongs.
            let b = s.app_root("B").unwrap();
            fs::create_dir_all(b.root().join("sub")).unwrap();
            symlink("elsewhere", b.root().join("sub/to_a")).unwrap();
            assert!(matches!(
                b.migrate_from_with(&old, links_on()),
                Err(AppRootError::Collision(_))
            ));
            assert_eq!(
                read_link_text(b.root().join("sub/to_a")),
                Path::new("elsewhere")
            );
            assert!(!b.root().join(MIGRATION_MARKER).exists());
            let c = s.app_root("C").unwrap();
            fs::create_dir_all(c.root().join("sub")).unwrap();
            fs::write(c.root().join("sub/to_a"), b"file").unwrap();
            assert!(matches!(
                c.migrate_from_with(&old, links_on()),
                Err(AppRootError::Collision(_))
            ));
            assert_eq!(fs::read(c.root().join("sub/to_a")).unwrap(), b"file");
            // An identical link already there is accepted.
            let e = s.app_root("E").unwrap();
            fs::create_dir_all(e.root().join("sub")).unwrap();
            symlink("../a.txt", e.root().join("sub/to_a")).unwrap();
            migrated(e.migrate_from_with(&old, links_on()).unwrap());
        }

        #[test]
        fn interrupted_link_migration_restarts_cleanly() {
            let (d, s) = suite();
            let a = s.app_root("App").unwrap();
            let old = old_tree(d.path());
            symlink("../a.txt", old.join("sub/to_a")).unwrap();
            symlink("sub", old.join("top")).unwrap();
            // A crash after one file, one link and a partial temp file.
            fs::copy(old.join("a.txt"), a.root().join("a.txt")).unwrap();
            fs::create_dir_all(a.root().join("sub")).unwrap();
            symlink("../a.txt", a.root().join("sub/to_a")).unwrap();
            fs::write(a.root().join(format!("sub/b.bin{TMP_SUFFIX}")), b"par").unwrap();
            assert!(!a.root().join(MIGRATION_MARKER).exists());
            let (files, links, _, _) = migrated(a.migrate_from_with(&old, links_on()).unwrap());
            assert_eq!((files, links.len()), (2, 2));
            assert_eq!(fs::read(a.root().join("sub/b.bin")).unwrap().len(), 200_000);
            assert_eq!(read_link_text(a.root().join("top")), Path::new("sub"));
            assert_eq!(
                a.migrate_from_with(&old, links_on()).unwrap().outcome(),
                &MigrationOutcome::AlreadyMigrated
            );
        }

        #[test]
        fn sockets_and_fifos_are_skipped_and_reported_never_opened() {
            let (d, s) = suite();
            let a = s.app_root("App").unwrap();
            let old = old_tree(d.path());
            let _sock = std::os::unix::net::UnixListener::bind(old.join("s")).unwrap();
            let st = std::process::Command::new("mkfifo")
                .arg(old.join("f"))
                .status()
                .unwrap();
            assert!(st.success());
            let before = snapshot(&old);
            // Strict, and links alone, still refuse.
            for o in [MigrateOptions::strict(), links_on()] {
                assert!(matches!(
                    a.migrate_from_with(&old, o),
                    Err(AppRootError::UnsupportedSource(_))
                ));
            }
            assert!(!a.root().join(MIGRATION_MARKER).exists());
            let o = MigrateOptions::strict().skip_ephemeral_specials(true);
            let (files, links, specials, _) = migrated(a.migrate_from_with(&old, o).unwrap());
            assert_eq!((files, links.len()), (2, 0));
            assert_eq!(
                specials,
                vec![
                    SkippedSpecial {
                        path: "f".into(),
                        kind: SpecialKind::Fifo
                    },
                    SkippedSpecial {
                        path: "s".into(),
                        kind: SpecialKind::Socket
                    },
                ]
            );
            assert!(fs::symlink_metadata(a.root().join("f")).is_err());
            assert!(fs::symlink_metadata(a.root().join("s")).is_err());
            let v = marker_value(&a);
            assert_eq!(
                v["skipped_specials"][0],
                serde_json::json!({"path": "f", "kind": "fifo"})
            );
            assert_eq!(
                v["skipped_specials"][1],
                serde_json::json!({"path": "s", "kind": "socket"})
            );
            assert_eq!(snapshot(&old), before);
        }

        #[test]
        fn strict_default_still_refuses_links_and_specials() {
            let (d, s) = suite();
            let old = old_tree(d.path());
            symlink("a.txt", old.join("l")).unwrap();
            let before = snapshot(&old);
            for (i, o) in [
                MigrateOptions::default(),
                MigrateOptions::strict(),
                MigrateOptions::strict().skip_ephemeral_specials(true),
                MigrateOptions::strict().optional_links(|_| true),
            ]
            .into_iter()
            .enumerate()
            {
                let a = s.app_root(&format!("App{i}")).unwrap();
                assert!(matches!(
                    a.migrate_from_with(&old, o),
                    Err(AppRootError::UnsupportedSource(_))
                ));
                assert!(!a.root().join(MIGRATION_MARKER).exists());
            }
            let a = s.app_root("Plain").unwrap();
            assert!(matches!(
                a.migrate_from(&old),
                Err(AppRootError::UnsupportedSource(_))
            ));
            assert_eq!(snapshot(&old), before);
        }

        fn denied(_: &Path, _: &Path, _: bool) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        }

        #[test]
        fn required_link_failure_is_typed_retryable_and_writes_no_marker() {
            let (d, s) = suite();
            let a = s.app_root("App").unwrap();
            let old = old_tree(d.path());
            symlink("../a.txt", old.join("sub/to_a")).unwrap();
            symlink("../../ext", old.join("sub/ext")).unwrap();
            let before = snapshot(&old);
            let err = a.migrate_impl(&old, &links_on(), &denied).unwrap_err();
            assert!(err.is_retryable());
            let typed = err.link_not_created().expect("typed link failure");
            assert_eq!(typed.link, Path::new("sub/ext"));
            assert_eq!(typed.target, Path::new("../../ext"));
            assert_eq!(
                typed.resolved,
                fs::canonicalize(d.path()).unwrap().join("ext")
            );
            assert!(
                matches!(err, AppRootError::Io(_)),
                "carried by the existing Io variant"
            );
            assert!(
                !a.root().join(MIGRATION_MARKER).exists(),
                "no completed marker"
            );
            assert_eq!(snapshot(&old), before);
            // Retry once links work: the same call completes and writes the marker.
            let (files, links, _, omitted) =
                migrated(a.migrate_from_with(&old, links_on()).unwrap());
            assert_eq!((files, links.len(), omitted.len()), (2, 2, 0));
            assert!(a.root().join(MIGRATION_MARKER).is_file());
            assert_eq!(snapshot(&old), before);
        }

        #[test]
        fn optional_link_failure_is_omitted_and_reported_separately() {
            let (d, s) = suite();
            let a = s.app_root("App").unwrap();
            let old = old_tree(d.path());
            fs::create_dir(old.join("skills")).unwrap();
            symlink("../../shared/tool", old.join("skills/tool")).unwrap();
            let before = snapshot(&old);
            // A policy that does not name the link leaves it required.
            let other = links_on().optional_links(|p| p.starts_with("worktrees"));
            assert!(a
                .migrate_impl(&old, &other, &denied)
                .unwrap_err()
                .link_not_created()
                .is_some());
            assert!(!a.root().join(MIGRATION_MARKER).exists());
            let opts = links_on().optional_links(|p| p.starts_with("skills"));
            let report = a.migrate_impl(&old, &opts, &denied).unwrap();
            assert!(!report.complete());
            let (files, links, _, omitted) = migrated(report);
            assert_eq!((files, links.len()), (2, 0));
            assert_eq!(omitted.len(), 1);
            assert_eq!(omitted[0].path, Path::new("skills/tool"));
            assert_eq!(omitted[0].original_target, Path::new("../../shared/tool"));
            assert_eq!(
                omitted[0].resolved_target,
                fs::canonicalize(d.path()).unwrap().join("shared/tool")
            );
            assert!(fs::symlink_metadata(a.root().join("skills/tool")).is_err());
            let v = marker_value(&a);
            assert_eq!(v["omitted_optional_links"].as_array().unwrap().len(), 1);
            assert_eq!(v["links"], serde_json::json!([]));
            assert_eq!(v["complete"], serde_json::json!(false));
            assert_eq!(snapshot(&old), before);
        }
    }

    #[cfg(windows)]
    mod windows_links {
        use super::*;
        use std::os::windows::fs::{symlink_dir, symlink_file, FileTypeExt};

        /// Creating the source links needs the same privilege as the migration; without it there
        /// is nothing to migrate and the test says so.
        fn old_with_links(d: &Path) -> Option<PathBuf> {
            let old = old_tree(d);
            if symlink_file("a.txt", old.join("la")).is_err()
                || symlink_dir("sub", old.join("ld")).is_err()
            {
                eprintln!("skipped: symbolic links cannot be created (no privilege)");
                return None;
            }
            Some(old)
        }

        #[test]
        fn links_migrate_with_their_kind_or_report_the_privilege_failure() {
            let (d, s) = suite();
            let a = s.app_root("App").unwrap();
            let Some(old) = old_with_links(d.path()) else {
                return;
            };
            let before = snapshot(&old);
            match a.migrate_from_with(&old, links_on()) {
                Ok(report) => {
                    assert_eq!(report.migrated_links().len(), 2);
                    let la = fs::symlink_metadata(a.root().join("la"))
                        .unwrap()
                        .file_type();
                    let ld = fs::symlink_metadata(a.root().join("ld"))
                        .unwrap()
                        .file_type();
                    assert!(la.is_symlink_file() && ld.is_symlink_dir());
                    assert!(marker_value(&a)["links"].as_array().unwrap().len() == 2);
                }
                Err(e) => {
                    let typed = e.link_not_created().expect("typed link failure");
                    assert!(typed.link == Path::new("la") || typed.link == Path::new("ld"));
                    assert!(typed.target == Path::new("a.txt") || typed.target == Path::new("sub"));
                    assert!(e.is_retryable());
                    assert!(!a.root().join(MIGRATION_MARKER).exists());
                }
            }
            assert_eq!(snapshot(&old), before);
        }

        #[test]
        fn optional_links_are_migrated_or_omitted_never_dropped_silently() {
            let (d, s) = suite();
            let a = s.app_root("App").unwrap();
            let Some(old) = old_with_links(d.path()) else {
                return;
            };
            let opts = links_on().optional_links(|_| true);
            let r = a.migrate_from_with(&old, opts).unwrap();
            assert!(matches!(r.outcome(), MigrationOutcome::Migrated { .. }));
            assert_eq!(
                r.migrated_links().len() + r.omitted_optional_links().len(),
                2
            );
            assert_eq!(r.complete(), r.omitted_optional_links().is_empty());
            assert_eq!(
                marker_value(&a)["complete"],
                serde_json::json!(r.complete())
            );
        }
    }
}
