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
//! strict by default (symlinks, junctions and special files are refused); [`MigrateOptions`] opts
//! in to copying symlinks as links, skipping sockets and FIFOs, and skipping (never following)
//! Windows junctions and other reparse points.
//!
//! # Completion states and markers
//!
//! A migration is in one of three [`MigrationState`]s, written in the versioned state file
//! [`MIGRATION_STATE`] (version 2; the unversioned `{source, files, bytes}` marker is version 1):
//!
//! * `Incomplete`: written before the destination is first touched and left in place by any
//!   interruption or failure. The old root stays the authority; the destination is not usable. A
//!   rerun verifies and finishes the whole copy.
//! * `ReadyWithOmissions`: every required entry is in place; only entries the caller declared
//!   disposable were omitted (optional links that could not be created, reparse points skipped on
//!   request). Every omission is recorded. A rerun retries the omitted links only; it does not
//!   re-verify files the app may have changed since.
//! * `Ready`: everything migrated.
//!
//! The historical marker [`MIGRATION_MARKER`] is written only for `Ready`, after the state file, so
//! a reader of an earlier version never takes an incomplete or partial destination for a finished
//! move.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use rightkit_fs::{AtomicWriteOptions, AtomicWriter, FsError, ReplaceMode};

use crate::discovery::try_lock_exclusive;
use crate::suite::{make_private_dir, Suite, SuiteError};

/// The longest app name accepted, in bytes.
pub const MAX_APP_NAME_LEN: usize = 64;
/// The historical commit marker written at the app root once a migration is fully verified. Only a
/// [`MigrationState::Ready`] migration writes it; readers look only at `source`.
pub const MIGRATION_MARKER: &str = ".rightkit-migrated.json";
/// The versioned completion state of a migration (`Ready`, `ReadyWithOmissions` or `Incomplete`),
/// written at the app root before the destination is first touched and rewritten when the run
/// ends. Read it with [`AppRoot::migration_state`].
pub const MIGRATION_STATE: &str = ".rightkit-migration-state.json";
/// Version of the [`MIGRATION_STATE`] file. Version 1 is the unversioned marker.
const STATE_VERSION: u64 = 2;
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
    /// The source tree holds a symlink, junction or other reparse point, or special file that this
    /// migration mode refuses (strict mode refuses every one; an opt-in mode still refuses
    /// devices and other specials, and a junction or reparse point unless
    /// [`MigrateOptions::skip_reparse_points`] is on).
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

/// How far a migration into an app root got. Recorded in the versioned state file
/// ([`MIGRATION_STATE`]); read it with [`AppRoot::migration_state`] or
/// [`MigrationReport::state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MigrationState {
    /// Everything was migrated and verified. The historical marker [`MIGRATION_MARKER`] exists.
    Ready,
    /// Every required entry is in place. Only entries the caller declared disposable are missing,
    /// and each is recorded (optional links that could not be created, reparse points skipped on
    /// request). The destination is usable. The historical marker is not written, so a reader of
    /// an earlier version does not take it for a complete move; a rerun retries the omitted links.
    ReadyWithOmissions,
    /// The copy was started and not finished (interrupted, or stopped by a failure such as a
    /// required link the destination cannot create). The destination is not usable: the old root
    /// stays the authority. A rerun verifies and finishes the copy. No historical marker exists.
    Incomplete,
}

impl MigrationState {
    /// The name written in the state file: `ready`, `ready_with_omissions` or `incomplete`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::ReadyWithOmissions => "ready_with_omissions",
            Self::Incomplete => "incomplete",
        }
    }

    /// True when the destination may be used as the app's data root (`Ready` or
    /// `ReadyWithOmissions`).
    pub fn is_usable(self) -> bool {
        !matches!(self, Self::Incomplete)
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "ready" => Some(Self::Ready),
            "ready_with_omissions" => Some(Self::ReadyWithOmissions),
            "incomplete" => Some(Self::Incomplete),
            _ => None,
        }
    }
}

/// What [`AppRoot::migrate_from_with`] did: the [`MigrationOutcome`] plus what an opt-in
/// migration did with links, special files and reparse points. The lists describe this call's
/// work only (they are empty when the outcome is `NothingToMigrate`; the state file holds the
/// record of an earlier call). The one exception is a rerun that retries the omissions of a
/// `ReadyWithOmissions` migration: its `migrated_links` are the links created now, and
/// `omitted_optional_links` and `skipped_reparse_points` are what is still unresolved.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MigrationReport {
    outcome: MigrationOutcome,
    migrated_links: Vec<MigratedLink>,
    skipped_specials: Vec<SkippedSpecial>,
    omitted_optional_links: Vec<OmittedLink>,
    skipped_reparse_points: Vec<SkippedReparsePoint>,
    state: MigrationState,
}

impl MigrationReport {
    fn bare(outcome: MigrationOutcome, state: MigrationState) -> Self {
        Self {
            outcome,
            migrated_links: Vec::new(),
            skipped_specials: Vec::new(),
            omitted_optional_links: Vec::new(),
            skipped_reparse_points: Vec::new(),
            state,
        }
    }

    /// How far the migration got. A returned report is `Ready` or `ReadyWithOmissions`; an
    /// incomplete migration is an error, and its state is read with [`AppRoot::migration_state`].
    pub fn state(&self) -> MigrationState {
        self.state
    }

    /// Junctions and other Windows reparse points skipped on request (see
    /// [`MigrateOptions::skip_reparse_points`]). None is ever followed, copied or recreated.
    pub fn skipped_reparse_points(&self) -> &[SkippedReparsePoint] {
        &self.skipped_reparse_points
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

    /// True only for [`MigrationState::Ready`]: false when an optional link was omitted or a
    /// reparse point was skipped. Skipped ephemeral specials (sockets, FIFOs) do not make a
    /// migration incomplete. For `AlreadyMigrated` this is the value the earlier state recorded.
    pub fn complete(&self) -> bool {
        self.state == MigrationState::Ready
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

/// The kind of Windows reparse point skipped by a migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReparseKind {
    /// A junction (mount point) or volume mount point. Std reports it as a directory symlink, but
    /// it is not one: it is never recreated as a symlink, because that would change what it is.
    Junction,
    /// Any other reparse point that is not a symlink or an `AF_UNIX` socket (cloud placeholders,
    /// WSL links, app execution aliases, deduplicated files, ...).
    Other,
}

impl ReparseKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Junction => "junction",
            Self::Other => "other",
        }
    }
}

/// A reparse point that was skipped (never followed, opened, copied or recreated). Windows only:
/// see [`MigrateOptions::skip_reparse_points`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SkippedReparsePoint {
    /// Path relative to the migrated tree.
    pub path: PathBuf,
    pub kind: ReparseKind,
    /// The reparse tag (`IO_REPARSE_TAG_*`).
    pub tag: u32,
    /// Why it was skipped.
    pub reason: String,
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
    reparse_points: bool,
    optional_link: Option<OptionalLinkPolicy>,
    excluded: Vec<PathBuf>,
}

impl fmt::Debug for MigrateOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MigrateOptions")
            .field("links", &self.links)
            .field("ephemeral_specials", &self.ephemeral_specials)
            .field("reparse_points", &self.reparse_points)
            .field("optional_link_policy", &self.optional_link.is_some())
            .field("excluded", &self.excluded)
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
    /// file are still refused. On Windows an `AF_UNIX` socket file (a reparse point) counts as a
    /// socket.
    pub fn skip_ephemeral_specials(mut self, on: bool) -> Self {
        self.ephemeral_specials = on;
        self
    }

    /// Windows reparse-point policy. A junction (mount point) or any other reparse point that is
    /// neither a symlink nor an `AF_UNIX` socket is never followed, opened, copied or recreated:
    /// the migration cannot tell whether it leads out of the root, and recreating a junction as a
    /// symlink would change what it is. By default (and in strict mode) such an entry refuses the
    /// migration with [`AppRootError::UnsupportedSource`], before anything is written. With this
    /// option the entry is skipped and reported ([`MigrationReport::skipped_reparse_points`], and
    /// in the state file), and the result is [`MigrationState::ReadyWithOmissions`], never `Ready`:
    /// the caller has declared the entry disposable and the omission stays on record. A directory
    /// reparse point is never recursed into. No effect on other platforms (they have none).
    pub fn skip_reparse_points(mut self, on: bool) -> Self {
        self.reparse_points = on;
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

    /// Leave the entry at `rel` (a path relative to the migrated tree) and everything below it out
    /// of the copy. For state a lifecycle participant moves by its own means (an encrypted
    /// database and its key, moved by [`RootCutover`](crate::RootCutover) through the participant
    /// that owns it): copying such files as plain bytes would race the owner's relocation and
    /// collide with its destination. An excluded path is not planned, copied, verified or
    /// recorded; it is neither an omission nor a reason for the state to be `ReadyWithOmissions`,
    /// so the caller that excludes a path owns moving it. Exclusions match exact components;
    /// differently spelled case aliases are refused rather than silently skipped. `rel` must be relative and made only of
    /// normal components; anything else is ignored (it can match no entry).
    pub fn exclude(mut self, rel: impl Into<PathBuf>) -> Self {
        let rel = rel.into();
        if rel.components().next().is_some()
            && rel.components().all(|c| matches!(c, Component::Normal(_)))
        {
            self.excluded.push(rel);
        }
        self
    }

    pub fn links(&self) -> bool {
        self.links
    }

    pub fn ephemeral_specials(&self) -> bool {
        self.ephemeral_specials
    }

    pub fn reparse_points(&self) -> bool {
        self.reparse_points
    }

    fn is_strict(&self) -> bool {
        !self.links
            && !self.ephemeral_specials
            && !self.reparse_points
            && self.optional_link.is_none()
    }

    fn is_optional(&self, rel: &Path) -> bool {
        self.optional_link.as_ref().is_some_and(|f| f(rel))
    }

    /// Exact component exclusions preserve distinct names on case-sensitive volumes.
    /// Differently spelled case aliases fail closed rather than silently omitting material.
    fn is_excluded(&self, rel: &Path) -> Result<bool, AppRootError> {
        for ex in &self.excluded {
            if rel.starts_with(ex) {
                return Ok(true);
            }
            let mut have = rel.components();
            let ambiguous = ex.components().all(|want| match (want, have.next()) {
                (Component::Normal(w), Some(Component::Normal(h))) => {
                    w.to_string_lossy().to_lowercase() == h.to_string_lossy().to_lowercase()
                }
                _ => false,
            });
            if ambiguous {
                return Err(AppRootError::UnsupportedSource(rel.to_path_buf()));
            }
        }
        Ok(false)
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

    /// Copy the old root `src` into this app root, verify every byte, then commit. Restart-safe:
    /// files land under a temporary name and are renamed only when fully written; a rerun after
    /// any interruption re-copies missing or partial files, accepts files already identical, and
    /// refuses (never overwrites) a differing destination file. Directories, including empty
    /// ones, are migrated. The versioned state ([`MIGRATION_STATE`]) is `Incomplete` from before
    /// the destination is first touched until the copy is verified; the historical marker
    /// ([`MIGRATION_MARKER`]) is written last, and once present for this source the call is a
    /// no-op. `src` is never changed or deleted. Symlinks, junctions and special files in `src` are
    /// refused (strict); use [`AppRoot::migrate_from_with`] to opt in to links, ephemeral specials
    /// and skipped reparse points. Takes the writer lock.
    pub fn migrate_from(&self, src: &Path) -> Result<MigrationOutcome, AppRootError> {
        Ok(self
            .migrate_from_with(src, MigrateOptions::strict())?
            .into_outcome())
    }

    /// [`AppRoot::migrate_from`] with opt-in behaviour. Lock, restart, collision handling and the
    /// marker-last ordering are the same in every mode; `MigrateOptions::strict()` is exactly
    /// `migrate_from`. See [`MigrateOptions`] for how links, ephemeral specials and reparse points
    /// are handled. Returns a [`MigrationReport`] (its `outcome()` is what `migrate_from`
    /// returns).
    ///
    /// A required link that the destination cannot create returns the retryable
    /// [`AppRootError::link_not_created`] error and leaves the state `Incomplete`: the old tree is
    /// untouched, no historical marker exists, and the incomplete destination is never treated as
    /// migrated by this or an earlier version. A rerun finishes the copy. When the state is
    /// `ReadyWithOmissions` a rerun retries only the omitted links; it neither re-verifies files
    /// nor fails on files the app changed since.
    pub fn migrate_from_with(
        &self,
        src: &Path,
        opts: MigrateOptions,
    ) -> Result<MigrationReport, AppRootError> {
        self.migrate_impl(src, &opts, &make_link)
    }

    /// The completion state of the migration of `src` into this app root: `None` when no migration
    /// of `src` has started here (or `src` does not exist), otherwise what the last run left.
    /// `Incomplete` means the destination must not be used and the old root stays the authority. A
    /// historical marker written by an earlier version counts as `Ready` (as `ReadyWithOmissions`
    /// when it records `"complete": false`). A state file written by a newer version is an error
    /// (`AppRootError::Io`, `InvalidData`). Takes no lock and changes nothing.
    pub fn migration_state(&self, src: &Path) -> Result<Option<MigrationState>, AppRootError> {
        if !src.is_absolute() || src.components().any(|c| c == Component::ParentDir) {
            return Err(AppRootError::PathEscape(src.display().to_string()));
        }
        let csrc = match fs::canonicalize(src) {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        if let Some((state, _)) = self.read_state(&csrc)? {
            return Ok(Some(state));
        }
        Ok(self.legacy_marker(&csrc).map(|complete| {
            if complete {
                MigrationState::Ready
            } else {
                MigrationState::ReadyWithOmissions
            }
        }))
    }

    /// The state file when it records `csrc` as its source. An unreadable or unversioned file is
    /// not ours and counts as absent; a file of a newer version, or with a state this version does
    /// not know, is refused rather than guessed at.
    fn read_state(
        &self,
        csrc: &Path,
    ) -> Result<Option<(MigrationState, serde_json::Value)>, AppRootError> {
        let path = self.root.join(MIGRATION_STATE);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::InvalidData
                ) =>
            {
                return Ok(None)
            }
            Err(e) => return Err(e.into()),
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            return Ok(None);
        };
        let Some(version) = v.get("version").and_then(|n| n.as_u64()) else {
            return Ok(None);
        };
        if v.get("source").and_then(|s| s.as_str()) != csrc.to_str() {
            return Ok(None);
        }
        let state = v
            .get("state")
            .and_then(|s| s.as_str())
            .and_then(MigrationState::parse);
        match state {
            Some(state) if version <= STATE_VERSION => Ok(Some((state, v))),
            _ => Err(AppRootError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} was written by a newer version or holds an unknown state; refusing to guess",
                    path.display()
                ),
            ))),
        }
    }

    /// `Some(complete)` when the historical marker records `csrc` as its source.
    fn legacy_marker(&self, csrc: &Path) -> Option<bool> {
        let text = fs::read_to_string(self.root.join(MIGRATION_MARKER)).ok()?;
        let v = serde_json::from_str::<serde_json::Value>(&text).ok()?;
        if v.get("source").and_then(|s| s.as_str()) != csrc.to_str() {
            return None;
        }
        Some(v.get("complete").and_then(|c| c.as_bool()).unwrap_or(true))
    }

    fn write_state(&self, body: &serde_json::Value) -> Result<(), AppRootError> {
        write_json_atomic(&self.root.join(MIGRATION_STATE), body)?;
        Ok(())
    }

    fn migrate_impl(
        &self,
        src: &Path,
        opts: &MigrateOptions,
        maker: LinkMaker<'_>,
    ) -> Result<MigrationReport, AppRootError> {
        self.migrate_current(src, opts, maker, false)
    }

    /// Cutover must compare current authoritative bytes even if a previous copy was Ready.
    pub(crate) fn migrate_for_cutover(
        &self,
        src: &Path,
        opts: &MigrateOptions,
    ) -> Result<MigrationReport, AppRootError> {
        self.migrate_current(src, opts, &make_link, true)
    }

    fn migrate_current(
        &self,
        src: &Path,
        opts: &MigrateOptions,
        maker: LinkMaker<'_>,
        revalidate: bool,
    ) -> Result<MigrationReport, AppRootError> {
        if !src.is_absolute() || src.components().any(|c| c == Component::ParentDir) {
            return Err(AppRootError::PathEscape(src.display().to_string()));
        }
        let md = match fs::symlink_metadata(src) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(MigrationReport::bare(
                    MigrationOutcome::NothingToMigrate,
                    MigrationState::Ready,
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
        match self.read_state(&csrc)? {
            Some((MigrationState::Ready, state)) if !revalidate => {
                self.repair_marker(&csrc, &state);
                return Ok(MigrationReport::bare(
                    MigrationOutcome::AlreadyMigrated,
                    MigrationState::Ready,
                ));
            }
            Some((MigrationState::ReadyWithOmissions, state)) if !revalidate => {
                return self.retry_unresolved(&csrc, &state, maker);
            }
            // An earlier run did not finish: do the whole copy again.
            Some(_) => {}
            None => {
                if let Some(complete) = self.legacy_marker(&csrc).filter(|_| !revalidate) {
                    let state = if complete {
                        MigrationState::Ready
                    } else {
                        MigrationState::ReadyWithOmissions
                    };
                    return Ok(MigrationReport::bare(
                        MigrationOutcome::AlreadyMigrated,
                        state,
                    ));
                }
            }
        }
        // Plan first: a source this mode refuses fails here, before the destination is touched.
        let mut entries = Vec::new();
        collect(&csrc, Path::new(""), opts, &mut entries)?;
        if revalidate {
            // Old root remains authoritative until switch. A file removed there after an earlier
            // copy must not survive silently in the selected tree; preserve it and refuse cutover.
            let current: std::collections::BTreeSet<_> = entries.iter().map(Entry::rel).collect();
            let mut destination = Vec::new();
            let destination_opts = opts.clone().exclude(WRITER_LOCK);
            collect(&cdst, Path::new(""), &destination_opts, &mut destination)?;
            for entry in &destination {
                if !current.contains(entry.rel()) {
                    return Err(AppRootError::Collision(cdst.join(entry.rel())));
                }
            }
        }
        // Record the pending state before the first destination write, so that any interruption
        // (a crash included) leaves `Incomplete` on disk and never a state that reads as done.
        self.write_state(&state_json(
            MigrationState::Incomplete,
            csrc.to_str(),
            &Lists::default(),
            None,
        ))?;
        match self.copy_entries(&csrc, &entries, opts, maker) {
            Ok(done) => self.commit(&csrc, opts, done),
            Err(e) => {
                self.record_failure(&csrc, &e);
                Err(e)
            }
        }
    }

    /// Copy and verify every planned entry. Nothing here writes a marker or a completed state.
    fn copy_entries(
        &self,
        csrc: &Path,
        entries: &[Entry],
        opts: &MigrateOptions,
        maker: LinkMaker<'_>,
    ) -> Result<Lists, AppRootError> {
        let mut done = Lists::default();
        // (relative path, written target, is a directory link) for verification.
        let mut created: Vec<(PathBuf, PathBuf, bool)> = Vec::new();
        for entry in entries {
            match entry {
                Entry::Dir { rel } => {
                    // Validate the parent chain only: the leaf must be a real directory and is
                    // never followed if it is a link.
                    let to = self.path_checked(rel, false)?;
                    match fs::symlink_metadata(&to) {
                        Ok(m) if m.file_type().is_dir() => make_private_dir(&to)?,
                        Ok(_) => return Err(AppRootError::Collision(to)),
                        Err(e) if e.kind() == io::ErrorKind::NotFound => make_private_dir(&to)?,
                        Err(e) => return Err(e.into()),
                    }
                }
                Entry::File { rel, len } => {
                    done.files += 1;
                    done.bytes += len;
                    let from = csrc.join(rel);
                    let to = self.path(rel)?;
                    if let Some(parent) = to.parent() {
                        make_private_dir(parent)?;
                    }
                    match fs::symlink_metadata(&to) {
                        Ok(m) if regular_entry(&m) && same_content(&from, &to)? => {
                            continue;
                        }
                        Ok(_) => return Err(AppRootError::Collision(to)),
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e.into()),
                    }
                    let mut writer =
                        AtomicWriter::create_new_private(&to).map_err(io::Error::other)?;
                    let mut source =
                        rightkit_fs::open_read_nofollow(&from, &rightkit_fs::ReadOptions::new())
                            .map_err(io::Error::other)?;
                    io::copy(&mut source, writer.file_mut())?;
                    commit_migration_file(writer, &from, &to)?;
                }
                Entry::Link { rel, target, dir } => {
                    let (written, rewritten) = plan_link(csrc, rel, target);
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
                                    done.omitted.push(OmittedLink {
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
                    done.links.push(MigratedLink {
                        path: rel.clone(),
                        original_target: target.clone(),
                        written_target: written,
                        rewritten,
                    });
                }
                Entry::Special { rel, kind } => done.specials.push(SkippedSpecial {
                    path: rel.clone(),
                    kind: *kind,
                }),
                Entry::Reparse { rel, kind, tag } => done.reparse.push(SkippedReparsePoint {
                    path: rel.clone(),
                    kind: *kind,
                    tag: *tag,
                    reason: reparse_reason(*kind, *tag),
                }),
            }
        }
        for entry in entries {
            match entry {
                Entry::File { rel, .. } => {
                    let to = self.path(rel)?;
                    if !same_content(&csrc.join(rel), &to)? {
                        return Err(AppRootError::VerifyFailed(to));
                    }
                }
                Entry::Dir { rel } => {
                    let to = self.path_checked(rel, false)?;
                    if !fs::symlink_metadata(&to).is_ok_and(|m| m.file_type().is_dir()) {
                        return Err(AppRootError::VerifyFailed(to));
                    }
                }
                _ => {}
            }
        }
        for (rel, written, dir) in &created {
            // Compare with read_link only; a link is never followed.
            let to = self.path_checked(rel, false)?;
            if !link_ok(&to, written, *dir) {
                return Err(AppRootError::VerifyFailed(to));
            }
        }
        Ok(done)
    }

    /// The copy is verified: write the final state, then (only for `Ready`) the historical marker.
    /// The state goes first, so a crash between the two leaves a state new readers trust and no
    /// marker an old reader could mistake; the next call repairs the marker.
    fn commit(
        &self,
        csrc: &Path,
        opts: &MigrateOptions,
        done: Lists,
    ) -> Result<MigrationReport, AppRootError> {
        let state = done.state();
        self.write_state(&state_json(state, csrc.to_str(), &done, None))?;
        if state == MigrationState::Ready {
            let body = marker_json(
                csrc.to_str(),
                done.files,
                done.bytes,
                opts,
                &done.links,
                &done.specials,
                &done.omitted,
            );
            write_json_atomic(&self.root.join(MIGRATION_MARKER), &body)?;
        }
        Ok(MigrationReport {
            outcome: MigrationOutcome::Migrated {
                files: done.files,
                bytes: done.bytes,
            },
            migrated_links: done.links,
            skipped_specials: done.specials,
            omitted_optional_links: done.omitted,
            skipped_reparse_points: done.reparse,
            state,
        })
    }

    /// Best effort: leave `Incomplete` on disk with the reason, and the link that blocked it.
    fn record_failure(&self, csrc: &Path, e: &AppRootError) {
        let mut body = state_json(
            MigrationState::Incomplete,
            csrc.to_str(),
            &Lists::default(),
            Some(&e.to_string()),
        );
        if let (Some(l), Some(obj)) = (e.link_not_created(), body.as_object_mut()) {
            obj.insert(
                "unresolved".into(),
                serde_json::json!([{"path": lossy(&l.link), "kind": "link", "reason": l.reason}]),
            );
        }
        let _ = self.write_state(&body);
    }

    /// A `Ready` state without its historical marker (a crash between the two writes): write it.
    fn repair_marker(&self, csrc: &Path, state: &serde_json::Value) {
        let path = self.root.join(MIGRATION_MARKER);
        if path.exists() {
            return;
        }
        let l = Lists::from_state(state);
        let opts = MigrateOptions::strict().preserve_links(true);
        let body = marker_json(
            csrc.to_str(),
            l.files,
            l.bytes,
            &opts,
            &l.links,
            &l.specials,
            &l.omitted,
        );
        let _ = write_json_atomic(&path, &body);
    }

    /// Rerun of a `ReadyWithOmissions` migration: retry the omitted links only. Files are not
    /// compared again (the app has been running on the destination and may have changed them), and
    /// a link that still cannot be created, or whose destination path is now occupied, stays
    /// omitted. Skipped reparse points stay skipped. When nothing is left unresolved the state
    /// becomes `Ready` and the historical marker is written.
    fn retry_unresolved(
        &self,
        csrc: &Path,
        state: &serde_json::Value,
        maker: LinkMaker<'_>,
    ) -> Result<MigrationReport, AppRootError> {
        let prior = Lists::from_state(state);
        let mut next = Lists {
            files: prior.files,
            bytes: prior.bytes,
            links: prior.links,
            specials: prior.specials,
            omitted: Vec::new(),
            reparse: prior.reparse,
        };
        let mut created = Vec::new();
        for o in prior.omitted {
            match self.retry_link(csrc, &o, maker)? {
                Ok(link) => {
                    created.push(link.clone());
                    next.links.push(link);
                }
                Err(reason) => next.omitted.push(OmittedLink { reason, ..o }),
            }
        }
        let now = next.state();
        if !created.is_empty() || now == MigrationState::Ready {
            self.write_state(&state_json(now, csrc.to_str(), &next, None))?;
            if now == MigrationState::Ready {
                let opts = MigrateOptions::strict().preserve_links(true);
                let body = marker_json(
                    csrc.to_str(),
                    next.files,
                    next.bytes,
                    &opts,
                    &next.links,
                    &next.specials,
                    &next.omitted,
                );
                write_json_atomic(&self.root.join(MIGRATION_MARKER), &body)?;
            }
        }
        Ok(MigrationReport {
            outcome: MigrationOutcome::AlreadyMigrated,
            migrated_links: created,
            skipped_specials: Vec::new(),
            omitted_optional_links: next.omitted,
            skipped_reparse_points: next.reparse,
            state: now,
        })
    }

    /// Retry one omitted link. `Ok(Ok(link))` it exists now (created, or already identical),
    /// `Ok(Err(reason))` it stays omitted, `Err` a fault that is not about links being unavailable.
    fn retry_link(
        &self,
        csrc: &Path,
        o: &OmittedLink,
        maker: LinkMaker<'_>,
    ) -> Result<Result<MigratedLink, String>, AppRootError> {
        let from = csrc.join(&o.path);
        let md = match fs::symlink_metadata(&from) {
            Ok(m) if m.file_type().is_symlink() => m,
            Ok(_) => return Ok(Err("the source entry is no longer a link".to_string())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Err("the source link no longer exists".to_string()))
            }
            Err(e) => return Err(e.into()),
        };
        let target = fs::read_link(&from)?;
        let dir = link_is_dir(&md.file_type());
        let (written, rewritten) = plan_link(csrc, &o.path, &target);
        let to = self.path_checked(&o.path, false)?;
        match fs::symlink_metadata(&to) {
            Ok(m) => {
                let same = m.file_type().is_symlink()
                    && fs::read_link(&to).is_ok_and(|t| same_link_target(&t, &written));
                if !same {
                    return Ok(Err("the destination path is occupied".to_string()));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if let Some(parent) = to.parent() {
                    make_private_dir(parent)?;
                }
                if let Err(e) = maker(&written, &to, dir) {
                    if link_unavailable(&e) {
                        return Ok(Err(e.to_string()));
                    }
                    return Err(e.into());
                }
            }
            Err(e) => return Err(e.into()),
        }
        if !link_ok(&to, &written, dir) {
            return Err(AppRootError::VerifyFailed(to));
        }
        Ok(Ok(MigratedLink {
            path: o.path.clone(),
            original_target: target,
            written_target: written,
            rewritten,
        }))
    }
}

type LinkMaker<'a> = &'a dyn Fn(&Path, &Path, bool) -> io::Result<()>;

/// One thing found under the source tree.
enum Entry {
    /// A directory (recorded before its contents), so an empty one is migrated too.
    Dir {
        rel: PathBuf,
    },
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
    /// A Windows junction or other reparse point, recorded without following it. Only built on
    /// Windows.
    #[cfg_attr(not(windows), allow(dead_code))]
    Reparse {
        rel: PathBuf,
        kind: ReparseKind,
        tag: u32,
    },
}

impl Entry {
    fn rel(&self) -> &Path {
        match self {
            Self::Dir { rel }
            | Self::File { rel, .. }
            | Self::Link { rel, .. }
            | Self::Special { rel, .. }
            | Self::Reparse { rel, .. } => rel,
        }
    }
}

/// What a migration did or found, as it is recorded in the state file. `Default` is the empty
/// state of a run that has just started.
#[derive(Default)]
struct Lists {
    files: u64,
    bytes: u64,
    links: Vec<MigratedLink>,
    specials: Vec<SkippedSpecial>,
    omitted: Vec<OmittedLink>,
    reparse: Vec<SkippedReparsePoint>,
}

impl Lists {
    /// `Ready` unless a link was omitted or a reparse point skipped. Skipped ephemeral specials
    /// are recorded but do not count.
    fn state(&self) -> MigrationState {
        if self.omitted.is_empty() && self.reparse.is_empty() {
            MigrationState::Ready
        } else {
            MigrationState::ReadyWithOmissions
        }
    }

    /// Read the lists back from a state file (entries that do not parse are dropped).
    fn from_state(v: &serde_json::Value) -> Self {
        let text =
            |o: &serde_json::Value, k: &str| o.get(k).and_then(|s| s.as_str()).map(PathBuf::from);
        let items = |k: &str| {
            v.get(k)
                .and_then(|a| a.as_array())
                .cloned()
                .unwrap_or_default()
        };
        Self {
            files: v.get("files").and_then(|n| n.as_u64()).unwrap_or(0),
            bytes: v.get("bytes").and_then(|n| n.as_u64()).unwrap_or(0),
            links: items("links")
                .iter()
                .filter_map(|o| {
                    Some(MigratedLink {
                        path: text(o, "path")?,
                        original_target: text(o, "original_target")?,
                        written_target: text(o, "written_target")?,
                        rewritten: o
                            .get("rewritten")
                            .and_then(|b| b.as_bool())
                            .unwrap_or(false),
                    })
                })
                .collect(),
            specials: items("skipped_specials")
                .iter()
                .filter_map(|o| {
                    let kind = match o.get("kind").and_then(|k| k.as_str())? {
                        "socket" => SpecialKind::Socket,
                        "fifo" => SpecialKind::Fifo,
                        _ => return None,
                    };
                    Some(SkippedSpecial {
                        path: text(o, "path")?,
                        kind,
                    })
                })
                .collect(),
            omitted: items("omitted_optional_links")
                .iter()
                .filter_map(|o| {
                    Some(OmittedLink {
                        path: text(o, "path")?,
                        original_target: text(o, "original_target")?,
                        resolved_target: text(o, "resolved_target")?,
                        reason: o.get("reason").and_then(|r| r.as_str())?.to_string(),
                    })
                })
                .collect(),
            reparse: items("skipped_reparse_points")
                .iter()
                .filter_map(|o| {
                    let kind = match o.get("kind").and_then(|k| k.as_str())? {
                        "junction" => ReparseKind::Junction,
                        _ => ReparseKind::Other,
                    };
                    Some(SkippedReparsePoint {
                        path: text(o, "path")?,
                        kind,
                        tag: o.get("tag").and_then(|t| t.as_u64()).unwrap_or(0) as u32,
                        reason: o.get("reason").and_then(|r| r.as_str())?.to_string(),
                    })
                })
                .collect(),
        }
    }
}

/// The names the migration itself owns at the root of the source: a marker or state file of an
/// earlier migration of that root must not be copied over this root's own.
fn is_reserved_root_name(name: &std::ffi::OsStr) -> bool {
    let n = name.to_string_lossy();
    n == MIGRATION_MARKER || n == MIGRATION_STATE
}

/// Every directory, regular file, and under `opts` every symlink, ephemeral special and skipped
/// reparse point, below `base/rel`. Directories are entries too (before their contents), so an
/// empty directory is migrated. A symlink, junction or reparse point is never traversed. Without
/// the matching option a symlink, junction, reparse point or special file is refused. The
/// migration's own marker and state names are not entries at the root of the source.
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
        if rel.as_os_str().is_empty() && is_reserved_root_name(&name) {
            continue;
        }
        let r = rel.join(&name);
        if opts.is_excluded(&r)? {
            continue;
        }
        // Windows: classify a reparse point before anything else. `DirEntry::file_type` reports a
        // junction as a directory symlink and an `AF_UNIX` socket file as a plain file; neither
        // may be treated as what std calls it.
        #[cfg(windows)]
        {
            if let Some(kind) = reparse_of(&e, &base.join(&r))? {
                match kind {
                    Reparse::Symlink => {}
                    Reparse::Socket => {
                        if opts.ephemeral_specials {
                            out.push(Entry::Special {
                                rel: r,
                                kind: SpecialKind::Socket,
                            });
                            continue;
                        }
                        return Err(AppRootError::UnsupportedSource(base.join(&r)));
                    }
                    Reparse::Junction | Reparse::Other(_) => {
                        if opts.reparse_points {
                            let (kind, tag) = match kind {
                                Reparse::Other(tag) => (ReparseKind::Other, tag),
                                _ => (ReparseKind::Junction, IO_REPARSE_TAG_MOUNT_POINT),
                            };
                            out.push(Entry::Reparse { rel: r, kind, tag });
                            continue;
                        }
                        return Err(AppRootError::UnsupportedSource(base.join(&r)));
                    }
                }
            }
        }
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
            out.push(Entry::Dir { rel: r.clone() });
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

/// How a Windows entry that carries `FILE_ATTRIBUTE_REPARSE_POINT` is classified by its tag.
#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reparse {
    /// `IO_REPARSE_TAG_SYMLINK`: handled as a link (`preserve_links`) like any other symlink.
    Symlink,
    /// `IO_REPARSE_TAG_MOUNT_POINT`: a junction or volume mount point.
    Junction,
    /// `IO_REPARSE_TAG_AF_UNIX`: an `AF_UNIX` socket file.
    Socket,
    /// Any other tag.
    Other(u32),
}

#[cfg(windows)]
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
#[cfg(windows)]
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
#[cfg(windows)]
const IO_REPARSE_TAG_AF_UNIX: u32 = 0x8000_0023;

#[cfg(windows)]
fn classify_reparse_tag(tag: u32) -> Reparse {
    match tag {
        IO_REPARSE_TAG_SYMLINK => Reparse::Symlink,
        IO_REPARSE_TAG_MOUNT_POINT => Reparse::Junction,
        IO_REPARSE_TAG_AF_UNIX => Reparse::Socket,
        other => Reparse::Other(other),
    }
}

/// `None` when the entry is not a reparse point. `DirEntry::metadata` does not follow reparse
/// points, so the attribute is the entry's own; the tag is read without opening the entry.
#[cfg(windows)]
fn reparse_of(e: &fs::DirEntry, full: &Path) -> io::Result<Option<Reparse>> {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    if e.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        return Ok(None);
    }
    Ok(Some(classify_reparse_tag(reparse_tag(full)?)))
}

/// The reparse tag of the entry at `path` (`WIN32_FIND_DATAW::dwReserved0` of `FindFirstFileW`,
/// which describes the entry itself and does not follow it).
#[cfg(windows)]
fn reparse_tag(path: &Path) -> io::Result<u32> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{FindClose, FindFirstFileW, WIN32_FIND_DATAW};
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: `wide` is NUL-terminated and outlives the call, and `data` is a valid out pointer.
    let handle =
        unsafe { FindFirstFileW(PCWSTR(wide.as_ptr()), &mut data) }.map_err(io::Error::other)?;
    // SAFETY: `handle` came from `FindFirstFileW` above and is closed exactly once.
    let _ = unsafe { FindClose(handle) };
    Ok(data.dwReserved0)
}

fn reparse_reason(kind: ReparseKind, tag: u32) -> String {
    match kind {
        ReparseKind::Junction => {
            "junction or volume mount point: never followed, copied or recreated".to_string()
        }
        ReparseKind::Other => {
            format!("reparse point (tag {tag:#010x}): never followed, opened, copied or recreated")
        }
    }
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

fn links_json(links: &[MigratedLink]) -> serde_json::Value {
    serde_json::Value::Array(
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
    )
}

fn specials_json(specials: &[SkippedSpecial]) -> serde_json::Value {
    serde_json::Value::Array(
        specials
            .iter()
            .map(|s| serde_json::json!({"path": lossy(&s.path), "kind": s.kind.as_str()}))
            .collect(),
    )
}

fn omitted_json(omitted: &[OmittedLink]) -> serde_json::Value {
    serde_json::Value::Array(
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
    )
}

fn reparse_json(reparse: &[SkippedReparsePoint]) -> serde_json::Value {
    serde_json::Value::Array(
        reparse
            .iter()
            .map(|r| {
                serde_json::json!({
                    "path": lossy(&r.path),
                    "kind": r.kind.as_str(),
                    "tag": r.tag,
                    "reason": r.reason,
                })
            })
            .collect(),
    )
}

/// The historical commit marker body, written only for a `Ready` migration. A strict migration
/// writes exactly the historical shape (`source`, `files`, `bytes`); an opt-in one adds `links`,
/// `skipped_specials`, `omitted_optional_links` and `complete`. Readers of the old shape look only
/// at `source`, so both shapes stay readable by both versions. (`omitted_optional_links` is
/// empty and `complete` true whenever the real flow writes it; the completion state with
/// omissions lives in the state file.)
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
    obj.insert("links".into(), links_json(links));
    obj.insert("skipped_specials".into(), specials_json(specials));
    obj.insert("omitted_optional_links".into(), omitted_json(omitted));
    obj.insert(
        "complete".into(),
        serde_json::Value::Bool(omitted.is_empty()),
    );
    body
}

/// The versioned state file body (see [`MIGRATION_STATE`]): `version`, `state`, `source`, the
/// counts, every list, and `unresolved` (the omitted links and skipped reparse points, with their
/// reasons). `error` is the reason an `Incomplete` run stopped, when it stopped on one.
fn state_json(
    state: MigrationState,
    source: Option<&str>,
    l: &Lists,
    error: Option<&str>,
) -> serde_json::Value {
    let mut unresolved: Vec<serde_json::Value> = l
        .omitted
        .iter()
        .map(|o| serde_json::json!({"path": lossy(&o.path), "kind": "link", "reason": o.reason}))
        .collect();
    unresolved.extend(l.reparse.iter().map(|r| {
        serde_json::json!({"path": lossy(&r.path), "kind": "reparse_point", "reason": r.reason})
    }));
    let mut body = serde_json::json!({
        "version": STATE_VERSION,
        "state": state.as_str(),
        "source": source,
        "files": l.files,
        "bytes": l.bytes,
        "links": links_json(&l.links),
        "skipped_specials": specials_json(&l.specials),
        "omitted_optional_links": omitted_json(&l.omitted),
        "skipped_reparse_points": reparse_json(&l.reparse),
        "unresolved": unresolved,
    });
    if let (Some(e), Some(obj)) = (error, body.as_object_mut()) {
        obj.insert("error".into(), e.into());
    }
    body
}

/// Publish a copy without replacing a contender. An equal regular file permits a retry.
fn commit_migration_file(
    writer: AtomicWriter,
    source: &Path,
    target: &Path,
) -> Result<(), AppRootError> {
    match writer.commit() {
        Ok(()) => Ok(()),
        Err(FsError::AlreadyExists(_)) => {
            if regular_entry(&fs::symlink_metadata(target)?) && same_content(source, target)? {
                Ok(())
            } else {
                Err(AppRootError::Collision(target.to_path_buf()))
            }
        }
        Err(e) => Err(io::Error::other(e).into()),
    }
}

fn regular_entry(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    metadata.file_type().is_file()
}

/// The migration owns these regular metadata leaves, never a link or special file.
/// Returns whether a validated target already exists.
fn validate_metadata_target(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(m) if regular_entry(&m) => Ok(true),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "migration metadata is not a regular file: {}",
                path.display()
            ),
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Write owned metadata through exclusive staging; never open a fixed temporary name.
pub(crate) fn write_json_atomic(path: &Path, body: &serde_json::Value) -> io::Result<()> {
    let replacing = validate_metadata_target(path)?;
    let mut writer = AtomicWriter::create_with(
        path,
        AtomicWriteOptions {
            private: true,
            replace: if replacing {
                ReplaceMode::Rename
            } else {
                ReplaceMode::NoReplace
            },
            ..Default::default()
        },
    )
    .map_err(io::Error::other)?;
    writer
        .write_all(body.to_string().as_bytes())
        .map_err(io::Error::other)?;
    writer
        .commit_checked(|_| validate_metadata_target(path).map(|_| ()))
        .map_err(io::Error::other)
}

/// Whether `to` is a link (never followed) with the written target text and, on Windows, the
/// right file or directory kind.
fn link_ok(to: &Path, written: &Path, dir: bool) -> bool {
    match fs::symlink_metadata(to) {
        Ok(m) => {
            m.file_type().is_symlink()
                && fs::read_link(to).is_ok_and(|t| same_link_target(&t, written))
                && link_kind_matches(&m.file_type(), dir)
        }
        Err(_) => false,
    }
}

/// Byte-for-byte equality (stronger than a digest compare).
pub(crate) fn same_content(a: &Path, b: &Path) -> Result<bool, AppRootError> {
    let mut fa = rightkit_fs::open_read_nofollow(a, &rightkit_fs::ReadOptions::new())
        .map_err(io::Error::other)?;
    let mut fb = rightkit_fs::open_read_nofollow(b, &rightkit_fs::ReadOptions::new())
        .map_err(io::Error::other)?;
    if fa.metadata()?.len() != fb.metadata()?.len() {
        return Ok(false);
    }
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

    /// [`eventually`] for calls that take the writer lock themselves (the migrations): retries only `WriterBusy`, so
    /// every other outcome, including the errors a test expects, is returned at once.
    fn settled<T>(mut f: impl FnMut() -> Result<T, AppRootError>) -> Result<T, AppRootError> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match f() {
                Err(AppRootError::WriterBusy(_)) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(20))
                }
                other => return other,
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
        eventually(|| a.lock_writer()).unwrap();
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
            settled(|| a.migrate_from(&old)).unwrap(),
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
            settled(|| a.migrate_from(&old)).unwrap(),
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
            settled(|| a.migrate_from(&old)).unwrap(),
            MigrationOutcome::Migrated { files: 2, .. }
        ));
        assert_eq!(fs::read(a.root().join("sub/b.bin")).unwrap().len(), 200_000);
    }

    #[test]
    fn migration_never_opens_legacy_temps_hardlinked_to_source() {
        let (d, s) = suite();
        let a = s.app_root("App").unwrap();
        let old = old_tree(d.path());
        for name in ["a.txt", MIGRATION_STATE, MIGRATION_MARKER] {
            fs::hard_link(
                old.join("a.txt"),
                a.root().join(format!("{name}{TMP_SUFFIX}")),
            )
            .unwrap();
        }
        settled(|| a.migrate_from(&old)).unwrap();
        assert_eq!(fs::read(old.join("a.txt")).unwrap(), b"alpha");
        assert_eq!(fs::read(a.root().join("a.txt")).unwrap(), b"alpha");
        for name in ["a.txt", MIGRATION_STATE, MIGRATION_MARKER] {
            assert_eq!(
                fs::read(a.root().join(format!("{name}{TMP_SUFFIX}"))).unwrap(),
                b"alpha"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn migration_never_follows_legacy_temp_symlinks_into_source() {
        let (d, s) = suite();
        let a = s.app_root("App").unwrap();
        let old = old_tree(d.path());
        for name in ["a.txt", MIGRATION_STATE, MIGRATION_MARKER] {
            std::os::unix::fs::symlink(
                old.join("a.txt"),
                a.root().join(format!("{name}{TMP_SUFFIX}")),
            )
            .unwrap();
        }
        settled(|| a.migrate_from(&old)).unwrap();
        assert_eq!(fs::read(old.join("a.txt")).unwrap(), b"alpha");
        assert_eq!(fs::read(a.root().join("a.txt")).unwrap(), b"alpha");
        for name in ["a.txt", MIGRATION_STATE, MIGRATION_MARKER] {
            assert!(
                fs::symlink_metadata(a.root().join(format!("{name}{TMP_SUFFIX}")))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
    }

    #[test]
    fn migration_publication_preserves_a_destination_contender() {
        let (d, s) = suite();
        let a = s.app_root("App").unwrap();
        let old = old_tree(d.path());
        let source = old.join("a.txt");
        let target = a.root().join("a.txt");
        for contender in [b"other".as_slice(), b"alpha".as_slice()] {
            let mut writer = AtomicWriter::create_new_private(&target).unwrap();
            writer.write_all(b"alpha").unwrap();
            let temp = writer.temp_path().to_path_buf();
            // A contender arrives after staging, before the real production commit helper.
            fs::write(&target, contender).unwrap();
            let result = commit_migration_file(writer, &source, &target);
            if contender == b"alpha" {
                result.unwrap();
            } else {
                assert!(matches!(result, Err(AppRootError::Collision(p)) if p == target));
            }
            assert_eq!(fs::read(&target).unwrap(), contender);
            assert_eq!(fs::read(&source).unwrap(), b"alpha");
            assert!(!temp.exists());
            fs::remove_file(&target).unwrap();
        }
    }

    #[test]
    fn migration_preserves_native_filenames_that_resemble_fs_temps() {
        let (d, s) = suite();
        let a = s.app_root("App").unwrap();
        let old = old_tree(d.path());
        let rel = Path::new("sub/.record.rktmp-123-1-0");
        fs::write(old.join(rel), b"user data, not a migration temp").unwrap();
        assert!(matches!(
            settled(|| a.migrate_from(&old)).unwrap(),
            MigrationOutcome::Migrated { files: 3, .. }
        ));
        assert_eq!(
            fs::read(a.root().join(rel)).unwrap(),
            b"user data, not a migration temp"
        );
        assert_eq!(
            fs::read(old.join(rel)).unwrap(),
            b"user data, not a migration temp"
        );
        assert_eq!(
            settled(|| a.migrate_from(&old)).unwrap(),
            MigrationOutcome::AlreadyMigrated
        );
    }

    #[cfg(unix)]
    #[test]
    fn metadata_publication_refuses_symlink_leaves_without_touching_source() {
        let (d, s) = suite();
        let a = s.app_root("App").unwrap();
        let old = old_tree(d.path());
        for name in [MIGRATION_STATE, MIGRATION_MARKER] {
            let target = a.root().join(name);
            std::os::unix::fs::symlink(old.join("a.txt"), &target).unwrap();
            let error =
                write_json_atomic(&target, &serde_json::json!({"source": "old"})).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(fs::read(old.join("a.txt")).unwrap(), b"alpha");
        }
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

    fn state_value(a: &AppRoot) -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(a.root().join(MIGRATION_STATE)).unwrap()).unwrap()
    }

    /// What a reader of 0.3.0 does: it looks only at the historical marker, and a marker that
    /// records this source means the move is done.
    fn old_reader_sees_migrated(a: &AppRoot, old: &Path) -> bool {
        let Ok(text) = fs::read_to_string(a.root().join(MIGRATION_MARKER)) else {
            return false;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            return false;
        };
        v.get("source").and_then(|s| s.as_str()) == fs::canonicalize(old).unwrap().to_str()
    }

    fn entry_count(dir: &Path) -> usize {
        fs::read_dir(dir).unwrap().count()
    }

    #[test]
    fn empty_directories_are_migrated_in_every_mode() {
        let (d, s) = suite();
        let old = old_tree(d.path());
        fs::create_dir(old.join("empty")).unwrap();
        fs::create_dir_all(old.join("sub/deep/emptier")).unwrap();
        let before = snapshot(&old);
        for (name, opts) in [
            ("Strict", MigrateOptions::strict()),
            ("Links", links_on()),
            (
                "Everything",
                links_on()
                    .skip_ephemeral_specials(true)
                    .skip_reparse_points(true),
            ),
        ] {
            let a = s.app_root(name).unwrap();
            let r = a.migrate_from_with(&old, opts).unwrap();
            assert_eq!(
                r.outcome(),
                &MigrationOutcome::Migrated {
                    files: 2,
                    bytes: 200_005
                },
                "{name}: directories are not counted as files"
            );
            assert_eq!(r.state(), MigrationState::Ready, "{name}");
            for dir in ["empty", "sub/deep", "sub/deep/emptier"] {
                let p = a.root().join(dir);
                assert!(
                    fs::symlink_metadata(&p).unwrap().file_type().is_dir(),
                    "{name}: {dir}"
                );
            }
            assert_eq!(entry_count(&a.root().join("empty")), 0, "{name}");
            assert_eq!(entry_count(&a.root().join("sub/deep/emptier")), 0, "{name}");
            assert_eq!(
                settled(|| a.migrate_from_with(&old, MigrateOptions::strict()))
                    .unwrap()
                    .outcome(),
                &MigrationOutcome::AlreadyMigrated
            );
        }
        assert_eq!(snapshot(&old), before);
    }

    #[test]
    fn a_directory_collision_is_refused_and_leaves_the_move_incomplete_until_resolved() {
        let (d, s) = suite();
        let a = s.app_root("App").unwrap();
        let old = old_tree(d.path());
        fs::create_dir(old.join("empty")).unwrap();
        // A file where the source has a directory.
        fs::write(a.root().join("empty"), b"in the way").unwrap();
        assert!(matches!(
            a.migrate_from(&old),
            Err(AppRootError::Collision(_))
        ));
        assert_eq!(fs::read(a.root().join("empty")).unwrap(), b"in the way");
        // Incomplete, and an earlier reader cannot take it for a finished move.
        assert!(!a.root().join(MIGRATION_MARKER).exists());
        assert!(!old_reader_sees_migrated(&a, &old));
        assert_eq!(
            a.migration_state(&old).unwrap(),
            Some(MigrationState::Incomplete)
        );
        assert_eq!(state_value(&a)["state"], serde_json::json!("incomplete"));
        assert!(state_value(&a)["error"]
            .as_str()
            .unwrap()
            .contains("differs from source"));
        // Resolve it and rerun: the same call finishes and writes the marker last.
        fs::remove_file(a.root().join("empty")).unwrap();
        assert!(matches!(
            settled(|| a.migrate_from(&old)).unwrap(),
            MigrationOutcome::Migrated { files: 2, .. }
        ));
        assert!(a.root().join("empty").is_dir());
        assert_eq!(
            a.migration_state(&old).unwrap(),
            Some(MigrationState::Ready)
        );
        assert!(old_reader_sees_migrated(&a, &old));
        assert_eq!(state_value(&a)["state"], serde_json::json!("ready"));
    }

    #[test]
    fn an_interrupted_run_leaves_incomplete_and_the_rerun_retries_everything() {
        let (d, s) = suite();
        let a = s.app_root("App").unwrap();
        let old = old_tree(d.path());
        fs::create_dir(old.join("empty")).unwrap();
        // The state a killed process leaves: pending state, one finished file, one partial temp.
        a.write_state(&state_json(
            MigrationState::Incomplete,
            fs::canonicalize(&old).unwrap().to_str(),
            &Lists::default(),
            None,
        ))
        .unwrap();
        fs::copy(old.join("a.txt"), a.root().join("a.txt")).unwrap();
        fs::create_dir_all(a.root().join("sub")).unwrap();
        fs::write(a.root().join(format!("sub/b.bin{TMP_SUFFIX}")), b"par").unwrap();
        assert_eq!(
            a.migration_state(&old).unwrap(),
            Some(MigrationState::Incomplete)
        );
        assert!(!old_reader_sees_migrated(&a, &old));
        let r = settled(|| a.migrate_from_with(&old, links_on())).unwrap();
        assert!(matches!(
            r.outcome(),
            MigrationOutcome::Migrated { files: 2, .. }
        ));
        assert_eq!(r.state(), MigrationState::Ready);
        assert_eq!(fs::read(a.root().join("sub/b.bin")).unwrap().len(), 200_000);
        assert!(a.root().join("empty").is_dir());
        assert!(old_reader_sees_migrated(&a, &old));
        assert_eq!(state_value(&a)["state"], serde_json::json!("ready"));
    }

    #[test]
    fn a_ready_state_without_its_marker_is_repaired_by_the_next_call() {
        let (d, s) = suite();
        let a = s.app_root("App").unwrap();
        let old = old_tree(d.path());
        a.migrate_from(&old).unwrap();
        // A crash between the state write and the marker write.
        fs::remove_file(a.root().join(MIGRATION_MARKER)).unwrap();
        assert!(!old_reader_sees_migrated(&a, &old));
        assert_eq!(
            settled(|| a.migrate_from(&old)).unwrap(),
            MigrationOutcome::AlreadyMigrated
        );
        assert!(old_reader_sees_migrated(&a, &old));
    }

    #[test]
    fn a_newer_state_file_is_refused_and_an_unreadable_one_is_ignored() {
        let (d, s) = suite();
        let a = s.app_root("App").unwrap();
        let old = old_tree(d.path());
        let src = fs::canonicalize(&old).unwrap();
        let state = a.root().join(MIGRATION_STATE);
        let write = |v: serde_json::Value| fs::write(&state, v.to_string()).unwrap();
        // Another source's state is not ours.
        write(serde_json::json!({"version": 9, "state": "ready", "source": "/elsewhere"}));
        assert_eq!(a.migration_state(&old).unwrap(), None);
        // A newer version, and a state this version does not know, are refused, not guessed at.
        for v in [
            serde_json::json!({"version": 99, "state": "ready", "source": src.to_str()}),
            serde_json::json!({"version": 2, "state": "teleported", "source": src.to_str()}),
        ] {
            write(v);
            for r in [
                a.migration_state(&old).map(|_| ()),
                settled(|| a.migrate_from(&old)).map(|_| ()),
            ] {
                match r {
                    Err(AppRootError::Io(e)) => {
                        assert_eq!(e.kind(), io::ErrorKind::InvalidData)
                    }
                    other => panic!("expected InvalidData, got {other:?}"),
                }
            }
            assert!(!a.root().join("a.txt").exists(), "nothing was copied");
        }
        // Garbage and an unversioned object are not ours: treated as absent.
        for text in ["not json {", "{\"source\": \"x\"}"] {
            fs::write(&state, text).unwrap();
            assert_eq!(a.migration_state(&old).unwrap(), None);
        }
        assert!(matches!(
            settled(|| a.migrate_from(&old)).unwrap(),
            MigrationOutcome::Migrated { files: 2, .. }
        ));
    }

    #[test]
    fn the_migrations_own_marker_names_in_the_source_are_not_copied() {
        let (d, s) = suite();
        let a = s.app_root("App").unwrap();
        let old = old_tree(d.path());
        // The old root was itself the destination of an earlier migration.
        fs::write(old.join(MIGRATION_MARKER), br#"{"source":"/earlier"}"#).unwrap();
        fs::write(old.join(MIGRATION_STATE), b"not ours").unwrap();
        // Only the root of the source is reserved: a nested file of the same name is data.
        fs::write(old.join("sub").join(MIGRATION_MARKER), b"data").unwrap();
        let r = a.migrate_from(&old).unwrap();
        assert!(matches!(r, MigrationOutcome::Migrated { files: 3, .. }));
        assert_eq!(
            marker_value(&a)["source"].as_str(),
            fs::canonicalize(&old).unwrap().to_str()
        );
        assert_eq!(state_value(&a)["state"], serde_json::json!("ready"));
        assert_eq!(
            fs::read(a.root().join("sub").join(MIGRATION_MARKER)).unwrap(),
            b"data"
        );
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
            settled(|| a.migrate_from_with(&old, links_on().skip_ephemeral_specials(true)))
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
                settled(|| a.migrate_from_with(&old, links_on()))
                    .unwrap()
                    .outcome(),
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
                    settled(|| a.migrate_from_with(&old, o.clone())),
                    Err(AppRootError::UnsupportedSource(_))
                ));
            }
            assert!(!a.root().join(MIGRATION_MARKER).exists());
            let o = MigrateOptions::strict().skip_ephemeral_specials(true);
            let (files, links, specials, _) =
                migrated(settled(|| a.migrate_from_with(&old, o.clone())).unwrap());
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
                    settled(|| a.migrate_from_with(&old, o.clone())),
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
                migrated(settled(|| a.migrate_from_with(&old, links_on())).unwrap());
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
            let report = settled(|| a.migrate_impl(&old, &opts, &denied)).unwrap();
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
            // Ready with omissions: usable, recorded in the state file, and no historical marker
            // for an earlier version to take it for a complete move.
            assert_eq!(
                a.migration_state(&old).unwrap(),
                Some(MigrationState::ReadyWithOmissions)
            );
            assert!(!a.root().join(MIGRATION_MARKER).exists());
            assert!(!old_reader_sees_migrated(&a, &old));
            let v = state_value(&a);
            assert_eq!(v["state"], serde_json::json!("ready_with_omissions"));
            assert_eq!(v["omitted_optional_links"].as_array().unwrap().len(), 1);
            assert_eq!(v["unresolved"][0]["path"], serde_json::json!("skills/tool"));
            assert_eq!(v["links"], serde_json::json!([]));
            assert_eq!(snapshot(&old), before);
        }

        #[test]
        fn omitted_optional_links_are_retried_and_the_state_becomes_ready() {
            let (d, s) = suite();
            let a = s.app_root("App").unwrap();
            let old = old_tree(d.path());
            fs::create_dir(old.join("skills")).unwrap();
            symlink("../../shared/tool", old.join("skills/tool")).unwrap();
            let before = snapshot(&old);
            let opts = || links_on().optional_links(|p| p.starts_with("skills"));
            let first = a.migrate_impl(&old, &opts(), &denied).unwrap();
            assert_eq!(first.state(), MigrationState::ReadyWithOmissions);
            // Still denied: the rerun retries, finds the link still omitted and changes nothing.
            let again = settled(|| a.migrate_impl(&old, &opts(), &denied)).unwrap();
            assert_eq!(again.outcome(), &MigrationOutcome::AlreadyMigrated);
            assert_eq!(again.state(), MigrationState::ReadyWithOmissions);
            assert_eq!(again.omitted_optional_links().len(), 1);
            assert!(again.migrated_links().is_empty());
            assert!(!a.root().join(MIGRATION_MARKER).exists());
            // The app has been running on the destination: a file it changed must not turn the
            // retry into a collision, because files are not compared again.
            fs::write(a.root().join("a.txt"), b"edited by the app").unwrap();
            let done = settled(|| a.migrate_from_with(&old, opts())).unwrap();
            assert_eq!(done.outcome(), &MigrationOutcome::AlreadyMigrated);
            assert_eq!(done.state(), MigrationState::Ready);
            assert!(done.complete() && done.omitted_optional_links().is_empty());
            assert_eq!(done.migrated_links().len(), 1);
            assert_eq!(done.migrated_links()[0].path, Path::new("skills/tool"));
            assert!(done.migrated_links()[0].rewritten);
            assert_eq!(
                read_link_text(a.root().join("skills/tool")),
                fs::canonicalize(d.path()).unwrap().join("shared/tool")
            );
            assert_eq!(
                fs::read(a.root().join("a.txt")).unwrap(),
                b"edited by the app"
            );
            assert_eq!(state_value(&a)["state"], serde_json::json!("ready"));
            assert_eq!(state_value(&a)["links"].as_array().unwrap().len(), 1);
            assert_eq!(state_value(&a)["unresolved"], serde_json::json!([]));
            assert!(old_reader_sees_migrated(&a, &old), "now a complete move");
            // Ready is final.
            let last = settled(|| a.migrate_from_with(&old, opts())).unwrap();
            assert_eq!(last.outcome(), &MigrationOutcome::AlreadyMigrated);
            assert!(last.complete() && last.migrated_links().is_empty());
            assert_eq!(snapshot(&old), before);
        }

        #[test]
        fn a_retry_never_overwrites_an_occupied_destination_path() {
            let (d, s) = suite();
            let a = s.app_root("App").unwrap();
            let old = old_tree(d.path());
            fs::create_dir(old.join("skills")).unwrap();
            symlink("tool-a", old.join("skills/tool")).unwrap();
            let opts = || links_on().optional_links(|p| p.starts_with("skills"));
            let first = a.migrate_impl(&old, &opts(), &denied).unwrap();
            assert_eq!(first.state(), MigrationState::ReadyWithOmissions);
            // The app put its own file where the omitted link belongs.
            fs::write(a.root().join("skills/tool"), b"the app's own file").unwrap();
            let r = settled(|| a.migrate_from_with(&old, opts())).unwrap();
            assert_eq!(r.state(), MigrationState::ReadyWithOmissions);
            assert_eq!(r.omitted_optional_links().len(), 1);
            assert!(r.omitted_optional_links()[0].reason.contains("occupied"));
            assert_eq!(
                fs::read(a.root().join("skills/tool")).unwrap(),
                b"the app's own file"
            );
            assert!(!a.root().join(MIGRATION_MARKER).exists());
        }

        #[test]
        fn a_link_to_an_empty_directory_does_not_dangle_in_the_new_tree() {
            let (d, s) = suite();
            let a = s.app_root("App").unwrap();
            let old = old_tree(d.path());
            fs::create_dir(old.join("empty")).unwrap();
            symlink("empty", old.join("to_empty")).unwrap();
            let before = snapshot(&old);
            let r = settled(|| a.migrate_from_with(&old, links_on())).unwrap();
            assert_eq!(r.state(), MigrationState::Ready);
            assert!(a.root().join("empty").is_dir());
            assert!(
                a.root().join("to_empty").is_dir(),
                "the link resolves to the migrated empty directory"
            );
            assert_eq!(
                read_link_text(a.root().join("to_empty")),
                Path::new("empty")
            );
            assert_eq!(snapshot(&old), before);
        }

        #[test]
        fn a_failed_required_link_leaves_incomplete_and_the_rerun_reaches_ready() {
            let (d, s) = suite();
            let a = s.app_root("App").unwrap();
            let old = old_tree(d.path());
            symlink("../a.txt", old.join("sub/to_a")).unwrap();
            symlink("../../ext", old.join("sub/ext")).unwrap();
            assert_eq!(a.migration_state(&old).unwrap(), None);
            let err = a.migrate_impl(&old, &links_on(), &denied).unwrap_err();
            assert!(err.is_retryable());
            // The old root stays the authority: no historical marker, an old reader (which looks
            // only at the marker) does not see a finished move, and the state says why.
            assert!(!a.root().join(MIGRATION_MARKER).exists());
            assert!(!old_reader_sees_migrated(&a, &old));
            assert_eq!(
                a.migration_state(&old).unwrap(),
                Some(MigrationState::Incomplete)
            );
            assert!(!a.migration_state(&old).unwrap().unwrap().is_usable());
            let v = state_value(&a);
            assert_eq!(v["state"], serde_json::json!("incomplete"));
            assert_eq!(v["version"], serde_json::json!(2));
            assert_eq!(v["unresolved"][0]["path"], serde_json::json!("sub/ext"));
            assert_eq!(v["unresolved"][0]["kind"], serde_json::json!("link"));
            assert!(v["error"]
                .as_str()
                .unwrap()
                .contains("cannot create symlink"));
            // A second denied attempt is the same error and the same state, not a success.
            assert!(settled(|| a.migrate_impl(&old, &links_on(), &denied)).is_err());
            assert_eq!(
                a.migration_state(&old).unwrap(),
                Some(MigrationState::Incomplete)
            );
            // Links work now: the rerun finishes the whole copy.
            let r = settled(|| a.migrate_from_with(&old, links_on())).unwrap();
            assert_eq!(r.state(), MigrationState::Ready);
            assert_eq!(r.migrated_links().len(), 2);
            assert_eq!(
                a.migration_state(&old).unwrap(),
                Some(MigrationState::Ready)
            );
            assert!(a.root().join(MIGRATION_MARKER).is_file());
            assert!(old_reader_sees_migrated(&a, &old));
            assert_eq!(state_value(&a)["state"], serde_json::json!("ready"));
        }

        #[test]
        fn socket_entry_is_skipped_and_recorded_without_making_the_move_incomplete() {
            let (d, s) = suite();
            let a = s.app_root("App").unwrap();
            let old = old_tree(d.path());
            let _sock = std::os::unix::net::UnixListener::bind(old.join("s")).unwrap();
            let before = snapshot(&old);
            let o = MigrateOptions::strict().skip_ephemeral_specials(true);
            let r = a.migrate_from_with(&old, o).unwrap();
            // A socket is ephemeral by nature: recorded, never opened or copied, and not an
            // omission that holds the migration back.
            assert_eq!(r.state(), MigrationState::Ready);
            assert!(r.complete());
            assert_eq!(r.skipped_specials().len(), 1);
            assert_eq!(r.skipped_specials()[0].kind, SpecialKind::Socket);
            assert!(r.skipped_reparse_points().is_empty());
            assert!(fs::symlink_metadata(a.root().join("s")).is_err());
            let v = state_value(&a);
            assert_eq!(
                v["skipped_specials"][0],
                serde_json::json!({"path": "s", "kind": "socket"})
            );
            assert_eq!(v["state"], serde_json::json!("ready"));
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
            let want = if r.complete() {
                "ready"
            } else {
                "ready_with_omissions"
            };
            assert_eq!(state_value(&a)["state"], serde_json::json!(want));
            // The historical marker exists exactly when nothing was omitted.
            assert_eq!(
                a.root().join(MIGRATION_MARKER).exists(),
                r.complete(),
                "an earlier reader sees a finished move only for a complete one"
            );
        }

        /// A junction needs no privilege (`mklink /J`). Without it there is nothing to test.
        fn junction(link: &Path, target: &Path) -> bool {
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .is_ok_and(|o| o.status.success())
        }

        #[test]
        fn reparse_tags_are_classified() {
            assert_eq!(classify_reparse_tag(0xA000_000C), Reparse::Symlink);
            assert_eq!(classify_reparse_tag(0xA000_0003), Reparse::Junction);
            assert_eq!(classify_reparse_tag(0x8000_0023), Reparse::Socket);
            // OneDrive placeholder and WSL symlink tags are neither: never followed, never copied.
            assert_eq!(
                classify_reparse_tag(0x9000_001A),
                Reparse::Other(0x9000_001A)
            );
            assert_eq!(
                classify_reparse_tag(0xA000_001D),
                Reparse::Other(0xA000_001D)
            );
        }

        #[test]
        fn a_junction_is_never_followed_copied_or_recreated() {
            let (d, s) = suite();
            let outside = d.path().join("outside");
            fs::create_dir(&outside).unwrap();
            fs::write(outside.join("secret.txt"), b"not part of the app root").unwrap();
            let old = old_tree(d.path());
            if !junction(&old.join("junc"), &outside) {
                eprintln!("skipped: cannot create a junction here");
                return;
            }
            let before = snapshot(&old);
            // Strict, links alone and specials alone all refuse, before anything is written.
            for (i, o) in [
                MigrateOptions::strict(),
                links_on(),
                MigrateOptions::strict().skip_ephemeral_specials(true),
            ]
            .into_iter()
            .enumerate()
            {
                let a = s.app_root(&format!("Refuse{i}")).unwrap();
                assert!(matches!(
                    a.migrate_from_with(&old, o),
                    Err(AppRootError::UnsupportedSource(_))
                ));
                assert!(!a.root().join(MIGRATION_MARKER).exists());
                assert!(!a.root().join(MIGRATION_STATE).exists());
                assert!(!a.root().join("a.txt").exists(), "nothing was copied");
                assert_eq!(a.migration_state(&old).unwrap(), None);
            }
            // Opt-in: skipped and reported with a reason, never followed or linked.
            let a = s.app_root("Skip").unwrap();
            let o = links_on().skip_reparse_points(true);
            let r = a.migrate_from_with(&old, o.clone()).unwrap();
            assert_eq!(r.state(), MigrationState::ReadyWithOmissions);
            assert!(!r.complete());
            assert_eq!(
                r.outcome(),
                &MigrationOutcome::Migrated {
                    files: 2,
                    bytes: 200_005
                }
            );
            assert_eq!(r.skipped_reparse_points().len(), 1);
            let skipped = &r.skipped_reparse_points()[0];
            assert_eq!(skipped.path, Path::new("junc"));
            assert_eq!(skipped.kind, ReparseKind::Junction);
            assert!(skipped.reason.contains("never followed"));
            assert!(r.migrated_links().is_empty(), "not recreated as a symlink");
            assert!(fs::symlink_metadata(a.root().join("junc")).is_err());
            assert!(
                snapshot(a.root())
                    .iter()
                    .all(|(k, _)| !k.contains("secret") && !k.contains("junc")),
                "nothing behind the junction was copied"
            );
            // Recorded as unresolved, and no historical marker: an earlier reader does not see a
            // finished move.
            assert!(!a.root().join(MIGRATION_MARKER).exists());
            assert!(!old_reader_sees_migrated(&a, &old));
            let v = state_value(&a);
            assert_eq!(v["state"], serde_json::json!("ready_with_omissions"));
            assert_eq!(
                v["skipped_reparse_points"][0]["path"],
                serde_json::json!("junc")
            );
            assert_eq!(
                v["skipped_reparse_points"][0]["kind"],
                serde_json::json!("junction")
            );
            assert_eq!(
                v["unresolved"][0]["kind"],
                serde_json::json!("reparse_point")
            );
            // A rerun keeps reporting it and stays ready with omissions.
            let again = a.migrate_from_with(&old, o).unwrap();
            assert_eq!(again.outcome(), &MigrationOutcome::AlreadyMigrated);
            assert_eq!(again.state(), MigrationState::ReadyWithOmissions);
            assert_eq!(again.skipped_reparse_points().len(), 1);
            assert_eq!(
                a.migration_state(&old).unwrap(),
                Some(MigrationState::ReadyWithOmissions)
            );
            // The old tree and the target behind the junction are untouched.
            assert_eq!(snapshot(&old), before);
            assert_eq!(
                fs::read(outside.join("secret.txt")).unwrap(),
                b"not part of the app root"
            );
        }
    }
}
