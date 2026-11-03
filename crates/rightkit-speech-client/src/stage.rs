//! Staging of input files the speech host may not be able to read.
//!
//! The host opens the path itself, in its own process. A path the caller can read may still
//! be denied to the host: on macOS a file on an external volume (under `/Volumes/`) needs a
//! grant the host may not hold. For a path the host cannot be expected to read, this client
//! copies the file into a client-owned staging directory under the per-user temporary
//! directory and submits the copy. The copy is removed when its request ends, however it
//! ends. `API.md`, "Input contract", states the rule.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{io_err, ClientError, ServiceErrorPayload};

/// The largest file the client stages by default: 4 GiB.
pub const DEFAULT_STAGE_LIMIT_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// The client-owned subdirectory of the per-user temporary directory.
const STAGING_DIR_NAME: &str = "rightkit-speech-client-staging";
const STAGE_ATTEMPTS: u32 = 8;

static STAGE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Whether an input path is copied to a staging file before it is submitted to the host.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StageInput {
    /// Stage a path the host cannot be expected to read; submit any other path unchanged.
    /// The default. The rule is in `API.md`, "Input contract".
    #[default]
    Auto,
    /// Always submit a staged copy.
    Always,
    /// Always submit the path as given, as the client did before staging existed.
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Platform {
    MacOs,
    Windows,
    Other,
}

impl Platform {
    pub(crate) fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

/// The directories the classifier and the staging step use. Tests inject their own.
#[derive(Debug, Clone)]
pub(crate) struct StageRoots {
    pub(crate) home: Option<PathBuf>,
    pub(crate) temp: PathBuf,
    pub(crate) platform: Platform,
}

impl StageRoots {
    pub(crate) fn system() -> Self {
        Self {
            home: std::env::home_dir(),
            temp: std::env::temp_dir(),
            platform: Platform::current(),
        }
    }

    fn staging_dir(&self) -> PathBuf {
        self.temp.join(STAGING_DIR_NAME)
    }
}

/// Whether the host can be expected to read `path` without a staged copy. The path must be
/// absolute, lie under the user's home directory or the per-user temporary directory, and be
/// on the same volume as that directory. On macOS nothing under `/Volumes/` qualifies. On
/// Windows the drive must match. Paths are compared after canonicalization when they exist.
/// A relative path never qualifies: the host resolves it in its own working directory.
pub(crate) fn host_readable_by_default(path: &Path, roots: &StageRoots) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let path = canonical_or(path);
    if roots.platform == Platform::MacOs && path.starts_with("/Volumes") {
        return false;
    }
    roots
        .home
        .iter()
        .chain(std::iter::once(&roots.temp))
        .map(|root| canonical_or(root))
        .any(|root| path.starts_with(&root) && same_drive(&path, &root, roots.platform))
}

fn canonical_or(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn same_drive(path: &Path, root: &Path, platform: Platform) -> bool {
    if platform != Platform::Windows {
        return true;
    }
    match (path.components().next(), root.components().next()) {
        (Some(Component::Prefix(a)), Some(Component::Prefix(b))) => {
            a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
        }
        _ => false,
    }
}

/// A staged copy. Dropping it removes the file, so the copy goes when its request ends,
/// on success, failure or cancel alike.
#[derive(Debug)]
pub(crate) struct Staged {
    path: PathBuf,
}

impl Staged {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Decide how `path` reaches the host. `Ok(None)` means submit `path` as given: the mode says
/// so, or there is no regular file to copy (the host then reports a missing path itself).
/// `Ok(Some)` is the staged copy, which the caller keeps alive until the request ends.
pub(crate) fn prepare(
    path: &Path,
    mode: StageInput,
    limit: u64,
    roots: &StageRoots,
) -> Result<Option<Staged>, ClientError> {
    let wanted = match mode {
        StageInput::Never => false,
        StageInput::Always => true,
        StageInput::Auto => !host_readable_by_default(path, roots),
    };
    if !wanted {
        return Ok(None);
    }
    let mut source = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(read_error(path, error)),
    };
    let meta = source.metadata().map_err(io_err)?;
    if !meta.is_file() {
        return Ok(None);
    }
    // Refuse before anything is created, so an over-limit file stages nothing.
    if meta.len() > limit {
        return Err(ClientError::StageLimitExceeded {
            path: path.to_path_buf(),
            bytes: meta.len(),
            limit,
        });
    }
    let (target, mut staged_file) = create_staged(path, roots)?;
    // From here the guard owns the file: any early return below removes it.
    let staged = Staged { path: target };
    let mut limited = (&mut source).take(limit.saturating_add(1));
    let copied = io::copy(&mut limited, &mut staged_file).map_err(|e| read_error(path, e))?;
    if copied > limit {
        return Err(ClientError::StageLimitExceeded {
            path: path.to_path_buf(),
            bytes: copied,
            limit,
        });
    }
    Ok(Some(staged))
}

/// A failed read of the caller's file is `InputUnreadable` (the process itself was denied);
/// any other failure is `Io`.
fn read_error(path: &Path, error: io::Error) -> ClientError {
    if error.kind() == io::ErrorKind::PermissionDenied {
        ClientError::InputUnreadable {
            path: path.to_path_buf(),
            message: format!("the client could not read the file to stage it: {error}"),
        }
    } else {
        io_err(error)
    }
}

fn create_staged(path: &Path, roots: &StageRoots) -> Result<(PathBuf, File), ClientError> {
    let dir = roots.staging_dir();
    ensure_private_dir(&dir).map_err(io_err)?;
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| !e.is_empty() && e.len() <= 8 && e.bytes().all(|b| b.is_ascii_alphanumeric()))
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    for _ in 0..STAGE_ATTEMPTS {
        let n = STAGE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let target = dir.join(format!(
            "stage-{}-{n}-{nanos}{extension}",
            std::process::id()
        ));
        if target.to_str().is_none() {
            return Err(ClientError::Unsupported(
                "the staging path is not valid UTF-8; the speech service's JSON wire carries UTF-8 paths only"
                    .into(),
            ));
        }
        match private_file_options().open(&target) {
            Ok(file) => return Ok((target, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(io_err(error)),
        }
    }
    Err(ClientError::Io(
        "could not create a unique staging file".into(),
    ))
}

#[cfg(unix)]
fn dir_builder() -> fs::DirBuilder {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder
}

#[cfg(not(unix))]
fn dir_builder() -> fs::DirBuilder {
    fs::DirBuilder::new()
}

/// Creates the staging directory if needed and checks it is a real directory. On Unix it is
/// owner-only (0700). On other platforms the per-user temporary directory is the boundary.
fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    match dir_builder().create(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(io::Error::other(
            "the staging path exists and is not a directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn private_file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Whether a host error says the host could not read its input. `E_PERMISSION` is the host's
/// own check. The host's FFmpeg and open-failure texts carry the same meaning under
/// `E_ENGINE`, so the message is read too. A decode failure on a readable file does not match.
pub(crate) fn host_could_not_read(error: &ServiceErrorPayload) -> bool {
    if error.code == "E_PERMISSION" {
        return true;
    }
    if error.code != "E_ENGINE" {
        return false;
    }
    let message = error.message.to_ascii_lowercase();
    let read_failure = message.starts_with("ffmpeg could not read this file")
        || message.starts_with("could not read ");
    read_failure
        && (message.contains("operation not permitted") || message.contains("permission denied"))
}
